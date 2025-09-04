# AI検索システム（Gemini + Brave API + Playwright）
# 必要ライブラリ: google-generativeai, requests, playwright

import asyncio
import requests
from dotenv import load_dotenv
import os
from playwright.async_api import async_playwright
import google.generativeai as genai

load_dotenv()

# --- 設定 ---
GOOGLE_API_KEY = os.environ.get("GOOGLE_API_KEY")
BRAVE_API_KEY = os.environ.get("BRAVE_API_KEY")

# Gemini初期化
genai.configure(api_key=GOOGLE_API_KEY)
model = genai.GenerativeModel(os.environ.get("GEMINI_MODEL"))

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
    response = model.generate_content(prompt)
    return response.text

# --- メイン処理 ---
async def ai_search(query):
    print(f"🔍 検索中: {query}\n")

    # 検索実行
    urls = search_brave(query)
    summaries = []

    # 各ページをPlaywrightで取得→Geminiで要約
    for url in urls:
        print(f"📄 要約中: {url}")
        summary = await fetch_and_summarize(url)
        if summary:
            summaries.append(f"### {url}\n{summary}\n")

    # 最終まとめ
    prompt = f"以下の情報を統合して、質問『{query}』に対して最も適切な回答を作ってください:\n\n" + "\n".join(summaries)
    final_response = model.generate_content(prompt)
    print("\n🧠 回答:\n")
    print(final_response.text)

# 実行用
if __name__ == '__main__':
    query = input("質問を入力してください: ")
    asyncio.run(ai_search(query))
