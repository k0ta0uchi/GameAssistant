import json
import requests

API_SERVER = "http://127.0.0.1:50021"


def save_speakers():
    """
    利用可能なSpeaker情報をファイル出力する
    """
    speakers = []

    response = requests.get(
        f"{API_SERVER}/speakers",
    )

    for item in json.loads(response.content):
        print(item)
        speaker = {
            "name": item["name"],
            "speaker_uuid": item["speaker_uuid"],
            "styles": [
                {"name": style["name"], "id": style["id"]}
                for style in item["styles"]
            ],
            "version": item["version"],
        }

        speakers.append(speaker)

    # 出力ファイルに保存
    with open("speakers.json", "w", encoding="utf-8") as f:
        json.dump(speakers, f, ensure_ascii=False, indent=4)


save_speakers()