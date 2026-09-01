import os
import json
import logging

class SettingsManager:
    def __init__(self, settings_file="settings.json"):
        if not os.path.isabs(settings_file):
            base_dir = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
            self.settings_file = os.path.join(base_dir, settings_file)
        else:
            self.settings_file = settings_file
        self.settings = self.load()

    def load(self):
        try:
            if os.path.exists(self.settings_file):
                with open(self.settings_file, "r", encoding="utf-8") as f:
                    return json.load(f)
            return {}
        except Exception as e:
            logging.warning(f"Settings load error ({self.settings_file}): {e}")
            return {}

    def save(self, settings_data=None):
        if settings_data is not None:
            self.settings = settings_data
        try:
            os.makedirs(os.path.dirname(self.settings_file), exist_ok=True)
            with open(self.settings_file, "w", encoding="utf-8") as f:
                json.dump(self.settings, f, ensure_ascii=False, indent=4)
        except Exception as e:
            logging.error(f"Settings save error ({self.settings_file}): {e}")

    def get(self, key, default=None):
        return self.settings.get(key, default)

    def set(self, key, value):
        self.settings[key] = value
        self.save()

