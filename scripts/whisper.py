from faster_whisper import WhisperModel

def recognize_speech(audio_file_path):
    """ 録音した音声をテキストに変換する """
    model = WhisperModel("large-v3", device="cuda", compute_type="int8")
    text = ""
    segments, info = model.transcribe(audio_file_path)
    for segment in segments:
        text += segment.text
    print("\n認識結果:", text)
    return text