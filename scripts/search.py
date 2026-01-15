# -*- coding: utf-8 -*-
# 高速版AI検索システム（Brave API + 軽量Playwright）
import requests
from dotenv import load_dotenv
import os
import time
import logging
import asyncio
from playwright.async_api import async_playwright
from .clients import get_gemini_client, switch_to_next_api_key

load_dotenv()

# --- 設定 ---
BRAVE_API_KEY = os.environ.get("BRAVE_API_KEY")
GEMINI_MODEL_NAME = os.environ.get("GEMINI_MODEL")

def _handle_quota_error() -> bool:
    """クォータエラー時にAPIキーを切り替える（search用）"""
    logging.warning("[Search] Gemini API Quota exhausted. Switching key...")
    if switch_to_next_api_key():
        logging.info("[Search] Switched to next API key.")
        return True
    return False

# --- 検索関数（Brave） ---
def search_brave(query, count=3): # デフォルトを3件に絞って高速化
    if not BRAVE_API_KEY:
        logging.warning("BRAVE_API_KEY is not set. Skipping web search.")
        return []
    url = "https://api.search.brave.com/res/v1/web/search"
    headers = {"Accept": "application/json", "X-Subscription-Token": BRAVE_API_KEY}
    params = {"q": query, "count": count}
    res = requests.get(url, headers=headers, params=params)
    res.raise_for_status()
    return [item["url"] for item in res.json().get("web", {}).get("results", [])]

# --- Webページを要約（Playwright 軽量モード） ---
async def fetch_and_summarize(browser_context, url):
    if not GEMINI_MODEL_NAME: raise ValueError("GEMINI_MODEL is not set.")
    
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
        text = await page.evaluate("""() => {
            const main = document.querySelector('main') || document.querySelector('article') || document.body;
            // 不要な要素を削除
            const scriptTags = main.querySelectorAll('script, style, nav, footer, header, noscript, iframe');
            scriptTags.forEach(s => s.remove());
            return main.innerText;
        }""")
        
    except Exception as e:
        logging.error(f"Error loading {url}: {e}")
        await page.close()
        return None
    
    await page.close()

    # 要約リクエスト
    prompt = f"以下の内容から重要な情報を抽出し、簡潔に箇条書きで要約してください:\n{text[:4000]}"
    
    while True:
        try:
            client = get_gemini_client()
            response = client.models.generate_content(model=GEMINI_MODEL_NAME, contents=prompt)
            return response.text
        except Exception as e:
            if ("429" in str(e) or "400" in str(e) or "ResourceExhausted" in str(e)) and _handle_quota_error():
                time.sleep(1)
                continue
            logging.error(f"Summarize failed for {url}: {e}")
            return None

# --- メイン処理 ---
async def ai_search(query):
    if not GEMINI_MODEL_NAME: raise ValueError("GEMINI_MODEL is not set.")
    logging.info(f"🔍 Web検索を開始: {query}")

    # キーワード変換
    prompt = f"検索エンジン用のキーワードに変換してください。キーワードのみ返してください：『{query}』"
    try:
        client = get_gemini_client()
        response = client.models.generate_content(model=GEMINI_MODEL_NAME, contents=prompt)
        keywords = response.text.strip()
    except Exception:
        keywords = query # フォールバック

    # 検索実行
    urls = search_brave(keywords)
    if not urls:
        return []

    summaries = []
    # Playwrightを1つのブラウザインスタンスで並列実行
    async with async_playwright() as p:
        browser = await p.chromium.launch(headless=True)
        # コンテキストを共有
        context = await browser.new_context(user_agent="Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36")
        
        logging.info(f"📄 {len(urls)} 件のページを並列解析中...")
        tasks = [fetch_and_summarize(context, url) for url in urls]
        summaries_raw = await asyncio.gather(*tasks)
        
        for url, summary in zip(urls, summaries_raw):
            if summary:
                summaries.append(f"### {url}\n{summary}\n")
        
        await browser.close()

    return summaries