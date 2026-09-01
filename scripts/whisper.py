# -*- coding: utf-8 -*-
import sys
import os
import io
import json
import base64
import logging
import torch
from faster_whisper import WhisperModel

# ログ設定 (stderr に出力)
logging.basicConfig(level=logging.INFO, format='[ASR-Native] %(message)s', stream=sys.stderr)

# GPU (CUDA) または CPU の判定
device = "cuda" if torch.cuda.is_available() else "cpu"
compute_type = "int8" if device == "cuda" else "int8"

logging.info(f"Native Faster-Whisper Engine initialized (device={device}, compute_type={compute_type})")

# モデルキャッシュ
models = {}

def get_model(model_name: str):
    if model_name in models:
        return models[model_name]

    if model_name == "tiny":
        target = "tiny"
        logging.info("Loading 'tiny' model...")
    else:
        local_path = os.path.abspath("models/kotoba-whisper-v2.0-faster")
        if os.path.exists(local_path):
            target = local_path
            logging.info(f"Loading local Kotoba-Whisper model from '{target}'...")
        else:
            target = "kotoba-tech/kotoba-whisper-v2.0-faster"
            logging.info(f"Local Kotoba model not found, loading from HF '{target}'...")

    try:
        model = WhisperModel(target, device=device, compute_type=compute_type)
        models[model_name] = model
        return model
    except Exception as e:
        logging.error(f"Failed to load model '{model_name}': {e}", exc_info=True)
        return None

# 初期ウォームアップ
get_model("kotoba")

logging.info("Native ASR Engine ready for inference.")

while True:
    try:
        line = sys.stdin.readline()
        if not line:
            break

        line = line.strip()
        if not line:
            continue

        req = json.loads(line)
        cmd = req.get("command", "transcribe")

        if cmd == "ping":
            sys.stdout.write(json.dumps({"status": "ok", "message": "pong"}) + "\n")
            sys.stdout.flush()
            continue

        if cmd == "transcribe":
            model_key = req.get("model", "kotoba")
            audio_b64 = req.get("audio_base64", "")
            
            if not audio_b64:
                sys.stdout.write(json.dumps({"status": "error", "error": "Empty audio data"}) + "\n")
                sys.stdout.flush()
                continue

            audio_bytes = base64.b64decode(audio_b64)
            audio_stream = io.BytesIO(audio_bytes)

            model = get_model(model_key)
            if model is None:
                sys.stdout.write(json.dumps({"status": "error", "error": f"Model '{model_key}' not loaded"}) + "\n")
                sys.stdout.flush()
                continue

            segments, _ = model.transcribe(
                audio_stream,
                language="ja",
                beam_size=1,
                vad_filter=True,
                vad_parameters=dict(min_silence_duration_ms=300),
            )

            text = "".join([s.text for s in segments]).strip()
            sys.stdout.write(json.dumps({"status": "ok", "text": text}) + "\n")
            sys.stdout.flush()

    except Exception as e:
        logging.error(f"Native ASR Error: {e}", exc_info=True)
        sys.stdout.write(json.dumps({"status": "error", "error": str(e)}) + "\n")
        sys.stdout.flush()