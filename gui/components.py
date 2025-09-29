import tkinter as tk
from tkinter import ttk, font, END, LEFT, X, BOTH, Y, RIGHT
import json
from datetime import datetime


class OutputRedirector:
    """print文をテキストボックスにリダイレクトするクラス"""
    def __init__(self, widget):
        self.widget = widget
        self.widget.tag_config("error", foreground="red")
        self.widget.tag_config("warning", foreground="yellow")
        self.widget.tag_config("success", foreground="green")
        self.widget.tag_config("info", foreground="cyan")

    def write(self, str):
        tag = None
        if "エラー" in str or "error" in str.lower():
            tag = "error"
        elif "警告" in str or "warning" in str.lower():
            tag = "warning"
        elif "成功" in str or "success" in str.lower() or "完了" in str:
            tag = "success"
        elif "***" in str:
            tag = "info"

        self.widget.insert(END, str, tag)
        self.widget.see(END)

    def flush(self):
        pass


class GeminiResponseWindow(tk.Toplevel):
    def __init__(self, parent, response_text, duration=10000):
        super().__init__(parent)
        self.title("Gemini Response")
        self.geometry("600x400")
        self.label = None
        self.create_widgets()
        self.configure(background="green")
        self.set_response_text(response_text) # Initialize with text
        self.after(duration, self.close_window) # Use close_window directly

    def create_widgets(self):
        my_font = font.Font(family='Arial', size=20)

        self.label = ttk.Label(
            self,
            text="",
            wraplength=600,
            justify=LEFT,
            background="green",
            foreground="white",
            padding=10,
            font=my_font,
            borderwidth=2,
        )
        self.label.pack(expand=True, fill=X)

    def set_response_text(self, response_text):
        if self.label:
            self.label.configure(text=response_text)

    def close_window(self):
        self.destroy()


class MemoryWindow(tk.Toplevel):
    def __init__(self, parent, memory_manager):
        super().__init__(parent)
        self.parent = parent
        self.memory_manager = memory_manager
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
        self.memory_listbox = ttk.Treeview(left_frame, columns=columns, show="headings")
        
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
                'user': meta.get('user', ''),
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
        
        selected_item = self.memory_listbox.item(selected_items[0])
        values = selected_item['values']
        
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
        key = self.key_entry.get()
        if not key:
            print("削除するキーを指定してください。")
            return
        if self.memory_manager.delete_memory(key):
            self.load_memories_to_listbox()
            self.clear_entries()
        else:
            print("指定されたキーのメモリーが見つかりません。")

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