# -*- coding: utf-8 -*-
# 高速・高精度AI検索システム（Grok風アーキテクチャ: 広域検索 + Re-ranking + Selected Scraping）
import requests
from dotenv import load_dotenv
import os
import time
import logging
import asyncio
import re
from playwright.async_api import async_playwright
import numpy as np
from datetime import datetime
import math
from sklearn.metrics.pairwise import cosine_similarity
from .memory import get_embedding_model
from .clients import get_gemini_client

load_dotenv()

# --- 設定 ---
BRAVE_API_KEY = os.environ.get("BRAVE_API_KEY")
GEMINI_MODEL = os.environ.get("GEMINI_MODEL")

def transform_query_to_keywords(query: str) -> str:
    """ユーザーの自然言語クエリを検索エンジン用キーワードに変換する"""
    if not GEMINI_MODEL:
        return query

    prompt = f"""
    以下のユーザーの質問から、検索エンジンに入力するための最適なキーワードだけを抽出してスペース区切りで出力してください。
    余計な説明や挨拶は不要です。

    ユーザーの質問: {query}
    キーワード:
    """
    
    try:
        logging.info(f"Transforming query to keywords: '{query[:50]}...'")
        client = get_gemini_client()
        # タイムアウトを短めに設定
        response = client.models.generate_content(
            model=GEMINI_MODEL,
            contents=prompt
        )
        if response and response.text:
            keywords = response.text.strip()
            # 「キーワード: 」などのプレフィックスが含まれる場合を除去
            keywords = re.sub(r'^(キーワード|Keywords)[:：\s]+', '', keywords, flags=re.I)
            logging.info(f"Transformation success: '{keywords}'")
            return keywords
        else:
            logging.warning("Gemini returned empty response for keywords transformation.")
            return query
    except Exception as e:
        logging.warning(f"Failed to transform query to keywords (using original): {e}")
        return query

class BraveSearchClient:
    def __init__(self, api_key):
        self.api_key = api_key
        self.base_url = "https://api.search.brave.com/res/v1/web/search"

    def search(self, query, count=50):
        if not self.api_key:
            logging.warning("BRAVE_API_KEY is not set.")
            return []
        
        headers = {
            "Accept": "application/json",
            "X-Subscription-Token": self.api_key
        }
        params = {
            "q": query,
            "count": count
        }
        
        try:
            res = requests.get(self.base_url, headers=headers, params=params)
            res.raise_for_status()
            data = res.json()
            return data.get("web", {}).get("results", [])
        except Exception as e:
            logging.error(f"Brave Search API failed: {e}")
            return []

async def fetch_page_content(browser_context, url):
    """Playwrightを使ってページの本文を抽出する"""
    page = await browser_context.new_page()
    try:
        # リソース制限（画像、CSS、フォントをブロック）
        async def block_aggressively(route):
            if route.request.resource_type in ["image", "stylesheet", "font", "media"]:
                await route.abort()
            else:
                await route.continue_()
        
        await page.route("**/*", block_aggressively)
        
        # タイムアウトを短めに設定 (10秒)
        await page.goto(url, timeout=10000, wait_until="domcontentloaded")
        
        # 本文の取得（主要なタグのみから抽出して精度と速度を上げる）
        # タイトルも取得して結合する
        content = await page.evaluate("""() => {
            const title = document.title;
            const main = document.querySelector('main') || document.querySelector('article') || document.body;
            
            // 不要な要素を削除
            const scriptTags = main.querySelectorAll('script, style, nav, footer, header, noscript, iframe, .ad, .ads, .social-share');
            scriptTags.forEach(s => s.remove());
            
            return `TITLE: ${title}\n\n${main.innerText}`;
        }""",
        )
        
        return content[:10000] # 文字数制限
        
    except Exception as e:
        logging.warning(f"Error loading {url}: {e}")
        return None
    finally:
        await page.close()

def calculate_freshness_score(date_str):
    """日付文字列から鮮度スコアを計算する (新しいほど高スコア)"""
    if not date_str:
        return 0.0
    try:
        # Braveの日付形式に対応 (例: "2023-10-27T...")
        # 形式が多様なため、簡易的なパースを試みる
        dt = None
        for fmt in ["%Y-%m-%d", "%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M:%SZ"]:
            try:
                dt = datetime.strptime(date_str.split('T')[0], "%Y-%m-%d")
                break
            except ValueError:
                continue
        
        if dt:
            days_old = (datetime.now() - dt).days
            if days_old < 0: days_old = 0
            # 減衰関数: 1年(365日)で約0.37倍になる指数減衰
            return math.exp(-days_old / 365.0)
    except Exception:
        pass
    return 0.0

async def ai_search(query):
    logging.info(f"🔍 AI Web Search (Grok-style) Started: {query}")
    
    # 0. クエリ変換 (Natural Language -> Search Keywords)
    search_keywords = transform_query_to_keywords(query)
    if not search_keywords or len(search_keywords.strip()) == 0:
        search_keywords = query
        logging.info("Using original query as search keywords.")

    # 1. 広範囲検索 (Brave Search API) using Keywords
    logging.info(f"Brave Search API Request: '{search_keywords}'")
    brave_client = BraveSearchClient(BRAVE_API_KEY)
    raw_results = brave_client.search(search_keywords, count=50)
    
    if not raw_results:
        logging.warning("Brave Search returned 0 results. Search aborted.")
        return []

    logging.info(f"Brave Search returned {len(raw_results)} results.")

    # 2. Re-ranking (Embedding + Cosine Similarity) using ORIGINAL Query
    logging.info("Calculating embeddings for re-ranking...")
    try:
        embedding_model = get_embedding_model()
        
        # クエリのベクトル化 (ユーザーの意図を汲むため元のクエリを使用)
        query_vec = embedding_model.encode(query, show_progress_bar=False)
        
        # ドキュメント（スニペット）のベクトル化
        # title + description を連結
        docs_text = [f"{item.get('title', '')} {item.get('description', '')}" for item in raw_results]
        docs_vecs = embedding_model.encode(docs_text, show_progress_bar=False) # バッチ処理
        
        # 類似度計算        # reshape(1, -1) で2次元配列にする
        similarities = cosine_similarity(query_vec.reshape(1, -1), docs_vecs)[0]
        
        scored_results = []
        for i, item in enumerate(raw_results):
            relevance_score = similarities[i]
            
            # 鮮度スコアの計算 (ageフィールドがある場合)
            freshness_score = 0.0
            if 'age' in item:
                freshness_score = calculate_freshness_score(item['age'])
            
            # 最終スコア: 関連度重視だが、鮮度も加味
            final_score = relevance_score * 0.8 + freshness_score * 0.2
            
            scored_results.append({
                "item": item,
                "score": final_score,
                "relevance": relevance_score
            })
        
        # ソートして上位5件を抽出
        top_results = sorted(scored_results, key=lambda x: x['score'], reverse=True)[:5]
        
        logging.info(f"Top 5 results selected (Score range: {top_results[0]['score']:.3f} - {top_results[-1]['score']:.3f})")
        for res in top_results:
            logging.info(f"- [{res['score']:.3f}] {res['item'].get('title')} ({res['item'].get('url')})")
    except Exception as e:
        logging.error(f"Re-ranking process failed: {e}", exc_info=True)
        # 失敗した場合はBraveの検索結果の上位5件をそのまま使う
        top_results = [{"item": item, "score": 0.0} for item in raw_results[:5]]
        logging.info("Falling back to top 5 results from Brave.")

    # 3. Selected Scraping (上位記事の本文取得)
    logging.info(f"Starting scraping for {len(top_results)} URLs...")
    urls = [res['item']['url'] for res in top_results]
    
    scraped_contents = []
    try:
        async with async_playwright() as p:
            browser = await p.chromium.launch(headless=True)
            context = await browser.new_context(
                user_agent="Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/91.0.4472.124 Safari/537.36"
            )
            
            tasks = [fetch_page_content(context, url) for url in urls]
            contents = await asyncio.gather(*tasks)
            
            for i, content in enumerate(contents):
                item = top_results[i]['item']
                if content and len(content.strip()) > 100:
                    logging.info(f"Successfully scraped content from: {item.get('url')} ({len(content)} chars)")
                    scraped_contents.append(f"### Source: {item.get('title')}\nURL: {item.get('url')}\n\n{content}\n")
                else:
                    logging.warning(f"Scraping yielded poor/no content for: {item.get('url')}. Using snippet instead.")
                    fallback = f"### Source: {item.get('title')}\nURL: {item.get('url')}\n(Note: Content fetch failed, using snippet)\n{item.get('description')}\n"
                    scraped_contents.append(fallback)
            
            await browser.close()
    except Exception as e:
        logging.error(f"Fatal error during scraping process: {e}", exc_info=True)
        # 全体的に失敗した場合は全件スニペットで返す
        for res in top_results:
            item = res['item']
            scraped_contents.append(f"### Source: {item.get('title')}\nURL: {item.get('url')}\n(Snippet only due to error)\n{item.get('description')}\n")

    logging.info(f"AI Search completed. Returning {len(scraped_contents)} sources.")
    return scraped_contents