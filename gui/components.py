import tkinter as tk
from tkinter import ttk, font, END, LEFT, X, BOTH, Y, RIGHT
import json
from datetime import datetime
import threading


class GeminiResponseWindow(tk.Toplevel):
    def __init__(self, parent, response_text, duration=10000):
        super().__init__(parent)
        self.title("Gemini Response")
        self.geometry("600x450")
        self.text_area = None
        self.duration = duration
        self.close_timer = None
        self.create_widgets()
        self.configure(background="#2b2b2b") # ダークテーマ風
        self.set_response_text(response_text)

    def create_widgets(self):
        self.text_area = tk.Text(
            self,
            wrap=tk.WORD,
            font=("Arial", 18),
            background="#2b2b2b",
            foreground="white",
            padx=20,
            pady=20,
            borderwidth=0,
            highlightthickness=0
        )
        self.text_area.pack(expand=True, fill=tk.BOTH)
        self.text_area.config(state="disabled")

    def set_response_text(self, response_text, auto_close=False):
        if self.text_area:
            self.text_area.config(state="normal")
            self.text_area.delete("1.0", tk.END)
            self.text_area.insert(tk.END, response_text)
            self.text_area.see(tk.END)
            self.text_area.config(state="disabled")
        
        if auto_close:
            self.start_close_timer()

    def start_close_timer(self):
        """表示終了タイマーを開始またはリセットする"""
        if self.close_timer:
            self.after_cancel(self.close_timer)
        self.close_timer = self.after(self.duration, self.close_window)

    def close_window(self):
        self.destroy()


class MemoryWindow(tk.Toplevel):
    def __init__(self, parent, app, memory_manager, gemini_service):
        super().__init__(parent)
        self.app = app
        self.memory_manager = memory_manager
        self.gemini_service = gemini_service
        self.title("メモリー管理")
        self.geometry("1000x600")

        self.create_widgets()
        self.load_memories_to_listbox()

    def create_widgets(self):
        main_frame = ttk.Frame(self, padding=10)
        main_frame.pack(fill=BOTH, expand=True)

        left_frame = ttk.Frame(main_frame)
        left_frame.pack(side=LEFT, fill=BOTH, expand=True, padx=(0, 10))

        columns = ("timestamp", "key", "type", "user", "comment")
        self.memory_listbox = ttk.Treeview(left_frame, columns=columns, show="headings", selectmode="extended")
        
        self.memory_listbox.heading("timestamp", text="タイムスタンプ", command=lambda: self.sort_column("timestamp", False))
        self.memory_listbox.heading("key", text="キー", command=lambda: self.sort_column("key", False))
        self.memory_listbox.heading("type", text="タイプ", command=lambda: self.sort_column("type", False))
        self.memory_listbox.heading("user", text="ユーザー", command=lambda: self.sort_column("user", False))
        self.memory_listbox.heading("comment", text="コメント", command=lambda: self.sort_column("comment", False))

        self.memory_listbox.column("timestamp", width=160, anchor='w')
        self.memory_listbox.column("key", width=120, anchor='w')
        self.memory_listbox.column("type", width=80, anchor='w')
        self.memory_listbox.column("user", width=80, anchor='w')
        self.memory_listbox.column("comment", width=400, anchor='w')

        self.memory_listbox.pack(fill=BOTH, expand=True)
        self.memory_listbox.bind("<<TreeviewSelect>>", self.on_memory_select)

        right_frame = ttk.Frame(main_frame, width=280)
        right_frame.pack(side=RIGHT, fill=Y)
        right_frame.pack_propagate(False)

        ttk.Label(right_frame, text="キー:").pack(fill=X, pady=(0, 5))
        self.key_entry = ttk.Entry(right_frame, state='readonly')
        self.key_entry.pack(fill=X, pady=(0, 10))

        ttk.Label(right_frame, text="タイムスタンプ:").pack(fill=X, pady=(0, 5))
        self.timestamp_label = ttk.Label(right_frame, text="")
        self.timestamp_label.pack(fill=X, pady=(0, 10))

        ttk.Label(right_frame, text="タイプ:").pack(fill=X, pady=(0, 5))
        self.type_entry = ttk.Entry(right_frame)
        self.type_entry.pack(fill=X, pady=(0, 10))

        ttk.Label(right_frame, text="ユーザー:").pack(fill=X, pady=(0, 5))
        self.user_entry = ttk.Entry(right_frame)
        self.user_entry.pack(fill=X, pady=(0, 10))

        ttk.Label(right_frame, text="コメント:").pack(fill=X, pady=(0, 5))
        self.comment_text = tk.Text(right_frame, height=10)
        self.comment_text.pack(fill=BOTH, expand=True, pady=(0, 10))

        button_frame = ttk.Frame(right_frame)
        button_frame.pack(fill=X, side='bottom')

        save_button = ttk.Button(button_frame, text="保存", command=self.save_memory, style="success.TButton")
        save_button.pack(side=LEFT, expand=True, fill=X, padx=(0, 5))

        delete_button = ttk.Button(button_frame, text="削除", command=self.delete_memory, style="danger.TButton")
        delete_button.pack(side=LEFT, expand=True, fill=X)

        generate_blog_button = ttk.Button(button_frame, text="ブログ生成", command=self.generate_blog_from_selection, style="info.TButton")
        generate_blog_button.pack(fill=X, pady=(5, 0))

    def sort_column(self, col, reverse):
        l = [(self.memory_listbox.set(k, col), k) for k in self.memory_listbox.get_children('')]
        l.sort(reverse=reverse)

        for index, (val, k) in enumerate(l):
            self.memory_listbox.move(k, '', index)

        self.memory_listbox.heading(col, command=lambda: self.sort_column(col, not reverse))

    def load_memories_to_listbox(self):
        for item in self.memory_listbox.get_children():
            self.memory_listbox.delete(item)
        
        memories_dict = self.memory_manager.get_all_memories()
        
        memory_list = []
        for key, value_json in memories_dict.items():
            try:
                value = json.loads(value_json)
                doc = value.get('document', '')
                meta = value.get('metadata', {})
            except (json.JSONDecodeError, AttributeError):
                doc = value_json # Not a JSON, treat as plain text
                meta = {}

            timestamp_str = meta.get('created_at') or meta.get('timestamp', '')
            if timestamp_str:
                try:
                    dt_obj = datetime.fromisoformat(timestamp_str)
                    display_ts = dt_obj.strftime('%Y-%m-%d %H:%M:%S')
                except (ValueError, TypeError):
                    display_ts = timestamp_str
            else:
                display_ts = "N/A"

            memory_list.append({
                'timestamp': timestamp_str,
                'display_ts': display_ts,
                'key': key,
                'type': meta.get('type', ''),
                'user': meta.get('source') or meta.get('user', ''),
                'comment': doc
            })

        memory_list.sort(key=lambda x: x['timestamp'], reverse=True)

        for mem in memory_list:
            self.memory_listbox.insert("", "end", values=(
                mem['display_ts'],
                mem['key'],
                mem['type'] or "",
                mem['user'] or "",
                mem['comment']
            ))

    def on_memory_select(self, event):
        selected_items = self.memory_listbox.selection()
        if not selected_items:
            return
        
        # 複数選択されている場合でも、最初のアイテムを詳細欄に表示
        first_item = self.memory_listbox.item(selected_items[0])
        values = first_item['values']
        
        display_ts, key, type_val, user_val, comment = values

        self.key_entry.config(state='normal')
        self.key_entry.delete(0, END)
        self.key_entry.insert(0, key)
        self.key_entry.config(state='readonly')

        self.timestamp_label.config(text=display_ts)
        
        self.type_entry.delete(0, END)
        self.type_entry.insert(0, str(type_val))
        
        self.user_entry.delete(0, END)
        self.user_entry.insert(0, str(user_val))
        
        self.comment_text.delete("1.0", END)
        self.comment_text.insert("1.0", comment)

    def generate_blog_from_selection(self):
        selected_items = self.memory_listbox.selection()
        if not selected_items:
            print("ブログを生成するメモリーが選択されていません。")
            return

        conversation_parts = []
        for item_id in selected_items:
            item = self.memory_listbox.item(item_id)
            values = item['values']
            timestamp, _, type_val, user, comment = values

            # 選択されたすべてのタイプをブログ生成のソースとして含める
            # 表示ラベルを調整
            label = user
            if type_val == 'twitch_chat':
                label = f"Twitch Viewer: {user}"
            elif type_val == 'ai_response':
                label = "AI"
            elif type_val == 'auto_commentary':
                label = "AI (Auto)"
            elif type_val == 'user_speech' or type_val == 'user_prompt':
                label = f"User: {user}"
            
            conversation_parts.append(f"[{timestamp}] {label}: {comment}")
        
        if not conversation_parts:
            print("ブログを生成するためのデータが見つかりませんでした。")
            return

        conversation = "\n\n".join(conversation_parts)

        # GameAssistantAppのブログ生成・保存メソッドを呼び出す
        # スレッドで実行してGUIが固まらないようにする
        threading.Thread(target=self.app.generate_and_save_blog_post, args=(conversation,)).start()


    def save_memory(self):
        key = self.key_entry.get()
        if not key:
            print("キーが選択されていません。")
            return

        comment = self.comment_text.get("1.0", END).strip()
        type_val = self.type_entry.get()
        user_val = self.user_entry.get()

        memories = self.memory_manager.get_all_memories()
        original_value_json = memories.get(key)
        
        created_at = None
        if original_value_json:
            try:
                original_value = json.loads(original_value_json)
                created_at = original_value.get('metadata', {}).get('created_at')
            except (json.JSONDecodeError, AttributeError):
                pass

        metadata = {'type': type_val, 'user': user_val}
        if created_at:
            metadata['created_at'] = created_at

        new_value_obj = {
            'document': comment,
            'metadata': metadata
        }
        new_value_json = json.dumps(new_value_obj, ensure_ascii=False, indent=2)

        self.memory_manager.add_or_update_memory(key, new_value_json)

        self.load_memories_to_listbox()
        self.clear_entries()

    def delete_memory(self):
        selected_items = self.memory_listbox.selection()
        if not selected_items:
            # リストボックスで選択されていない場合、入力欄のキーを削除試行（従来互換）
            key = self.key_entry.get()
            if not key:
                print("削除するメモリーを選択してください。")
                return
            if self.memory_manager.delete_memory(key):
                self.load_memories_to_listbox()
                self.clear_entries()
            else:
                print("指定されたキーのメモリーが見つかりません。")
            return

        # 複数選択削除
        deleted_count = 0
        for item_id in selected_items:
            item = self.memory_listbox.item(item_id)
            values = item['values']
            # valuesの2番目（インデックス1）がキー
            key = values[1]
            
            if self.memory_manager.delete_memory(key):
                deleted_count += 1
        
        print(f"{deleted_count} 件のメモリーを削除しました。")
        self.load_memories_to_listbox()
        self.clear_entries()

    def clear_entries(self):
        self.key_entry.config(state='normal')
        self.key_entry.delete(0, END)
        self.key_entry.config(state='readonly')
        
        self.timestamp_label.config(text="")
        self.type_entry.delete(0, END)
        self.user_entry.delete(0, END)
        self.comment_text.delete("1.0", END)
        
        selection = self.memory_listbox.selection()
        if selection:
            self.memory_listbox.selection_remove(selection)