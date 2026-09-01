from llama_cpp import Llama
from .prompts import MEMORY_SUMMARIZE_PROMPT, get_prompt

# グローバルにLLMインスタンスを保持
llm = None

def initialize_llm(model_path="./models/gemma-3-1b-it-Q4_K_S.gguf", n_ctx=2048, n_threads=8):
    """LLMを初期化する"""
    global llm
    if llm is None:
        try:
            llm = Llama(
                model_path=model_path,
                n_ctx=n_ctx,
                n_threads=n_threads,
                verbose=False
            )
            print("ローカルLLMの初期化に成功しました。")
        except Exception as e:
            print(f"ローカルLLMの初期化中にエラーが発生しました: {e}")
            llm = None # 初期化失敗時はNoneのままにする

def summarize(text: str, settings_manager=None) -> str:
    """テキストを1文で要約する"""
    if llm is None:
        print("LLMが初期化されていません。")
        return "要約できませんでした。"

    template = get_prompt("memory_summarize_prompt", settings_manager)
    if "{text}" in template:
        prompt = template.format(text=text)
    else:
        prompt = f"{template}\n\n発言: {text}\n記録:"

    try:
        result = llm(
            prompt,
            max_tokens=128,
            temperature=0.2,
            stop=["\n"]
        )
        return result["choices"][0]["text"].strip()
    except Exception as e:
        print(f"要約中にエラーが発生しました: {e}")
        return "要約中にエラーが発生しました。"

# テスト用
if __name__ == "__main__":
    initialize_llm()
    if llm:
        test_text = "昨日は猫カフェに行って、とても癒された。特に白い子猫がかわいかった。"
        summary = summarize(test_text)
        print(f"元のテキスト: {test_text}")
        print(f"要約結果: {summary}")