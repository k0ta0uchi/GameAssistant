# AI検索システム（Gemini + Brave API + Playwright）
# 必要ライブラリ: google-generativeai, requests, playwright

import requests
from dotenv import load_dotenv
import os
from playwright.async_api import async_playwright
from google import genai

load_dotenv()

# --- 設定 ---
GOOGLE_API_KEY = os.environ.get("GOOGLE_API_KEY")
BRAVE_API_KEY = os.environ.get("BRAVE_API_KEY")
GEMINI_MODEL_NAME = os.environ.get("GEMINI_MODEL")

# Geminiクライアント初期化
if not GOOGLE_API_KEY:
    raise ValueError("GOOGLE_API_KEY is not set.")
client = genai.Client(api_key=GOOGLE_API_KEY)

# --- 検索関数（Brave） ---
def search_brave(query, count=5):
    url = "https://api.search.brave.com/res/v1/web/search"
    headers = {
        "Accept": "application/json",
        "X-Subscription-Token": BRAVE_API_KEY,
    }
    params = {"q": query, "count": count}
    res = requests.get(url, headers=headers, params=params)
    res.raise_for_status()
    return [item["url"] for item in res.json().get("web", {}).get("results", [])]

# --- Webページを要約（Playwrightで取得） ---
async def fetch_and_summarize(url):
    if not GEMINI_MODEL_NAME:
        raise ValueError("GEMINI_MODEL is not set.")
    async with async_playwright() as p:
        browser = await p.chromium.launch()
        page = await browser.new_page()
        try:
            await page.goto(url, timeout=15000)
            content = await page.content()
            text = await page.inner_text("body")
        except Exception as e:
            print(f"Error loading {url}: {e}")
            return None
        await browser.close()

    prompt = f"以下の内容を簡潔に要約してください:\n{text[:5000]}"
    response = client.models.generate_content(model=GEMINI_MODEL_NAME, contents=prompt)
    return response.text

# --- メイン処理 ---
async def ai_search(query):
    if not GEMINI_MODEL_NAME:
        raise ValueError("GEMINI_MODEL is not set.")
    print(f"🔍 検索中: {query}\n")

    prompt = f"以下の自然文の質問を、検索エンジン向けのキーワードに変換してください。変換後のキーワードのみ返答してください：『{query}』"
    response = client.models.generate_content(model=GEMINI_MODEL_NAME, contents=prompt)
    keywords = response.text

    print(f"{keywords}")

    # 検索実行
    urls = search_brave(keywords)
    summaries = []

    # 各ページをPlaywrightで取得→Geminiで要約
    for url in urls:
        print(f"📄 要約中: {url}")
        summary = await fetch_and_summarize(url)
        if summary:
            summaries.append(f"### {url}\n{summary}\n")

    return summaries

    # # 最終まとめ
    # prompt = f"以下の情報を統合して、質問『{query}』に対して最も適切な回答を作ってください:\n\n" + "\n".join(summaries)
    # final_response = model.generate_content(prompt)
    # return final_response