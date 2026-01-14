import torch
import os
import logging

# もう使用しないため、ダミー関数として残すか、必要なら後で削除
def recognize_speech(audio_file_path: str) -> str:
    logging.warning("Deprecated: recognize_speech in scripts/whisper.py is no longer used.")
    return ""