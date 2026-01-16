# -*- coding: utf-8 -*-
import os
import json
import logging
import sys
import time
from typing import Optional, List
from fastapi import FastAPI, Request, Response, Query
from fastapi.responses import JSONResponse
import uvicorn
import torch
import numpy as np

# インポート前に環境変数を設定
MODEL_DIR = "models/vits2"
BERT_DIR = os.path.abspath(os.path.join(MODEL_DIR, "bert"))
os.environ["BERT_MODELS_DIR"] = BERT_DIR

# Style-Bert-VITS2 関連のインポート
try:
    from style_bert_vits2.tts_model import TTSModel
    from style_bert_vits2.constants import Languages
    from style_bert_vits2.nlp import bert_models
except ImportError as e:
    logging.critical(f"style-bert-vits2 のインポートに失敗しました: {e}", exc_info=True)
    sys.exit(1)

app = FastAPI(title="Style-Bert-VITS2 VOICEVOX Wrapper")

# モデル保持辞書 {speaker_id: TTSModel}
models = {}
# モデルのパス保持辞書 {speaker_id: {model_path, config_path, style_vec_path}}
model_configs_cache = {}
# スピーカー情報（VOICEVOX互換）
speakers_info = []
# BERTモデルがロード済みかどうかのフラグ
bert_loaded = False

def scan_models():
    """ディレクトリをスキャンして利用可能なモデルのリストを作成する"""
    global speakers_info, model_configs_cache
    speakers_info.clear()
    model_configs_cache.clear()

    if not os.path.exists(MODEL_DIR):
        os.makedirs(MODEL_DIR)
        return

    logging.info(f"モデルディレクトリをスキャン中: {MODEL_DIR}")
    speaker_id_counter = 0
    
    for model_name in os.listdir(MODEL_DIR):
        model_path = os.path.join(MODEL_DIR, model_name)
        if os.path.isdir(model_path) and model_name not in [".cache", "bert"]:
            config_file = os.path.join(model_path, "config.json")
            style_vec_path = os.path.join(model_path, "style_vectors.npy")
            
            if not os.path.exists(config_file) or not os.path.exists(style_vec_path):
                continue

            model_file = None
            for ext in [".safetensors", ".onnx"]:
                found_files = [f for f in os.listdir(model_path) if f.endswith(ext) and not f.startswith(("D_", "WD_"))]
                if found_files:
                    g_files = [f for f in found_files if f.startswith("G_")]
                    model_file = os.path.join(model_path, sorted(g_files)[-1] if g_files else found_files[0])
                    break
            
            if model_file:
                model_configs_cache[speaker_id_counter] = {
                    "model_path": model_file,
                    "config_path": config_file,
                    "style_vec_path": style_vec_path,
                    "name": model_name
                }
                # 話者リストに追加（わんコメ等の外部アプリ互換性のため拡張）
                speakers_info.append({
                    "name": model_name,
                    "speaker_uuid": f"vits2-{model_name}", # 簡易的なUUID
                    "styles": [
                        {
                            "name": "Normal", 
                            "id": speaker_id_counter,
                            "type": "talk" # 必須フィールド
                        }
                    ],
                    "version": "1.0.0",
                    "supported_features": {
                        "permitted_synthesis_morphing": "ALL" # 必須フィールド
                    }
                })
                speaker_id_counter += 1

    logging.info(f"スキャン完了。見つかったモデル数: {len(speakers_info)}")

def ensure_model_loaded(speaker_id: int):
    """リクエストされたモデルとBERTが必要な場合にロードする"""
    global bert_loaded, models
    start_time = time.time()
    
    # 1. BERTのロード
    if not bert_loaded:
        try:
            # os.sep を使用してパス区切り文字の問題を回避
            bert_pt_dir = os.path.relpath(os.path.join(BERT_DIR, "deberta-v2-large-japanese-char-wwm")).replace(os.sep, "/")
            logging.info(f"BERTモデル(PyTorch)のロードを開始します: {bert_pt_dir}")
            
            if os.path.exists(bert_pt_dir):
                bert_models.load_tokenizer(Languages.JP, bert_pt_dir)
                bert_models.load_model(Languages.JP, bert_pt_dir)
            else:
                logging.info("指定されたBERTパスが見つからないためデフォルトをロードします...")
                bert_models.load_bert_models()
            
            bert_loaded = True
            logging.info(f"BERTモデルロード完了 ({time.time() - start_time:.2f}秒)")
        except Exception as e:
            logging.error(f"BERTロード失敗: {e}")
            raise

    # 2. TTSモデルのロード
    if speaker_id not in models:
        if speaker_id not in model_configs_cache:
            raise ValueError(f"Speaker ID {speaker_id} not found.")
            
        conf = model_configs_cache[speaker_id]
        model_start_time = time.time()
        device = "cuda" if torch.cuda.is_available() else "cpu"
        logging.info(f"モデル '{conf['name']}' を {device} にロード中...")
        
        try:
            model = TTSModel(
                model_path=conf["model_path"],
                config_path=conf["config_path"],
                style_vec_path=conf["style_vec_path"],
                device=device
            )
            
            # torch.compile (PyTorch 2.0+ & CUDA)
            if hasattr(torch, "compile") and device == "cuda":
                try:
                    logging.info("モデル最適化中 (torch.compile)...")
                    # TTSModel 内部の Generator (net_g) をコンパイル
                    model.net_g = torch.compile(model.net_g)
                    logging.info("最適化が有効になりました。")
                except Exception as e:
                    logging.warning(f"最適化失敗（スキップ）: {e}")
        except Exception as e:
            logging.error(f"モデルロード中にエラーが発生しました: {e}")
            raise e

        # Warm-up (初回の推論遅延を防止)
        try:
            logging.info("暖機運転中 (Warm-up)...")
            model.infer(text="わん！", language=Languages.JP, speaker_id=0)
        except Exception as e:
            logging.warning(f"暖機運転中にエラー: {e}")

        models[speaker_id] = model
        logging.info(f"モデル '{conf['name']}' 準備完了 ({time.time() - model_start_time:.2f}秒)")
    
    total_time = time.time() - start_time
    if total_time > 0.5:
        logging.info(f"ロードプロセス終了 (総計: {total_time:.2f}秒)")

@app.get("/speakers")
async def get_speakers():
    return speakers_info

@app.post("/initialize")
def initialize_model(speaker: int = Query(0)):
    try:
        ensure_model_loaded(speaker)
        return {"status": "success"}
    except Exception as e:
        logging.error(f"事前ロード失敗: {e}")
        return JSONResponse(status_code=500, content={"detail": str(e)})

@app.post("/audio_query")
def audio_query(text: str, speaker: int):
    return {
        "text": text,
        "speaker_id": speaker,
        "speedScale": 1.0,
        "pitchScale": 0.0,
        "intonationScale": 1.0,
        "volumeScale": 1.0,
        "outputSamplingRate": 44100
    }

@app.post("/synthesis")
async def synthesis(request: Request, speaker: int):
    # synthesisのみ、bodyを非同期で受け取るため async def のままにする
    # ただし、中の重い処理はブロックしないよう配慮
    try:
        query_data = await request.json()
        text = query_data.get("text", "")
        speed_scale = query_data.get("speedScale", 1.0)

        # ロードと推論を同期的に実行
        ensure_model_loaded(speaker)
        
        if speaker not in models:
             raise RuntimeError(f"Speaker ID {speaker} not loaded.")
             
        model = models[speaker]
        
        # デバッグログ：使用中のモデル名を確認
        model_name = model_configs_cache.get(speaker, {}).get("name", "Unknown")
        logging.info(f"合成に使用中のモデル: {model_name} (ID: {speaker})")
        
        sr, wav = model.infer(
            text=text,
            language=Languages.JP,
            speaker_id=0,
            length=1.0 / speed_scale if speed_scale > 0 else 1.0
        )
        
        # 正規化（ノイズ対策）
        wav = wav.astype(np.float32)
        max_val = np.abs(wav).max()
        if max_val > 0:
            wav = (wav / max_val) * 0.9
        
        wav_int16 = (wav * 32767).astype(np.int16)
        
        import io
        import scipy.io.wavfile as wavfile
        byte_io = io.BytesIO()
        wavfile.write(byte_io, sr, wav_int16)
        return Response(content=byte_io.getvalue(), media_type="audio/wav")
    except Exception as e:
        logging.error(f"合成エラー: {e}", exc_info=True)
        return JSONResponse(status_code=500, content={"detail": str(e)})

if __name__ == "__main__":
    logging.basicConfig(level=logging.INFO)
    scan_models()
    uvicorn.run(app, host="127.0.0.1", port=50021)
