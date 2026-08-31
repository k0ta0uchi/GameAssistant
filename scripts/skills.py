# -*- coding: utf-8 -*-
import os
import re
import logging
from typing import List, Dict, Optional

logger = logging.getLogger(__name__)

def parse_skill_file(file_path: str) -> Optional[Dict[str, str]]:
    """
    SKILL.md ファイルを解析し、メタデータと本文を返します。
    """
    if not os.path.exists(file_path):
        return None
    try:
        with open(file_path, "r", encoding="utf-8") as f:
            content = f.read()

        # YAML frontmatter の抽出
        frontmatter_match = re.match(r"^---\s*\n(.*?)\n---\s*\n(.*)$", content, re.DOTALL)
        
        name = ""
        description = ""
        body = content

        if frontmatter_match:
            frontmatter_text = frontmatter_match.group(1)
            body = frontmatter_match.group(2).strip()

            for line in frontmatter_text.splitlines():
                if ":" in line:
                    key, val = line.split(":", 1)
                    key = key.strip().lower()
                    val = val.strip().strip("'\"")
                    if key == "name":
                        name = val
                    elif key == "description":
                        description = val

        dir_name = os.path.basename(os.path.dirname(os.path.abspath(file_path)))
        skill_id = dir_name if dir_name != "skills" else os.path.splitext(os.path.basename(file_path))[0]

        if not name:
            name = skill_id

        return {
            "id": skill_id,
            "name": name,
            "description": description,
            "content": body,
            "full_content": content,
            "file_path": os.path.abspath(file_path)
        }
    except Exception as e:
        logger.error(f"Error parsing skill file {file_path}: {e}")
        return None

def get_available_skills(skills_dir: str = "skills") -> List[Dict[str, str]]:
    """
    skills ディレクトリ配下の全スキルを取得します。
    - skills/{skill_dir}/SKILL.md
    - skills/{skill_name}.md
    """
    skills = []
    if not os.path.exists(skills_dir):
        return skills

    # サブディレクトリ内の SKILL.md を検索
    for item in os.listdir(skills_dir):
        item_path = os.path.join(skills_dir, item)
        if os.path.isdir(item_path):
            skill_md = os.path.join(item_path, "SKILL.md")
            if os.path.exists(skill_md):
                skill_info = parse_skill_file(skill_md)
                if skill_info:
                    skills.append(skill_info)
        elif item.endswith(".md"):
            skill_info = parse_skill_file(item_path)
            if skill_info:
                skills.append(skill_info)

    return skills

def load_enabled_skills_content(enabled_skill_ids: List[str], skills_dir: str = "skills") -> str:
    """
    有効化されたスキルIDに対応するスキルの内容を結合して返します。
    """
    if not enabled_skill_ids:
        return ""

    available = {s["id"]: s for s in get_available_skills(skills_dir)}
    
    sections = []
    for skill_id in enabled_skill_ids:
        if skill_id in available:
            skill = available[skill_id]
            section = f"## スキル: {skill['name']} ({skill['id']})\n"
            if skill.get("description"):
                section += f"> 説明: {skill['description']}\n\n"
            section += skill["content"]
            sections.append(section)

    return "\n\n---\n\n".join(sections)
