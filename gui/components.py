import tkinter as tk
from tkinter import ttk, font, END, LEFT, X, BOTH, Y, RIGHT


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
        self.geometry("500x400")

        self.create_widgets()
        self.load_memories_to_listbox()

    def create_widgets(self):
        main_frame = ttk.Frame(self, padding=10)
        main_frame.pack(fill=BOTH, expand=True)

        left_frame = ttk.Frame(main_frame)
        left_frame.pack(side=LEFT, fill=BOTH, expand=True, padx=(0, 10))

        self.memory_listbox = ttk.Treeview(left_frame, columns=("key", "value"), show="headings")
        self.memory_listbox.heading("key", text="キー")
        self.memory_listbox.heading("value", text="値")
        self.memory_listbox.pack(fill=BOTH, expand=True)
        self.memory_listbox.bind("<<TreeviewSelect>>", self.on_memory_select)

        right_frame = ttk.Frame(main_frame, width=200)
        right_frame.pack(side=RIGHT, fill=Y)
        right_frame.pack_propagate(False)

        key_label = ttk.Label(right_frame, text="キー:")
        key_label.pack(fill=X, pady=(0, 5))
        self.key_entry = ttk.Entry(right_frame)
        self.key_entry.pack(fill=X, pady=(0, 10))

        value_label = ttk.Label(right_frame, text="値:")
        value_label.pack(fill=X, pady=(0, 5))
        self.value_text = tk.Text(right_frame, height=5) # Use tk.Text
        self.value_text.pack(fill=BOTH, expand=True, pady=(0, 10))

        button_frame = ttk.Frame(right_frame)
        button_frame.pack(fill=X)

        save_button = ttk.Button(button_frame, text="保存", command=self.save_memory, style="success.TButton")
        save_button.pack(side=LEFT, expand=True, fill=X, padx=(0, 5))

        delete_button = ttk.Button(button_frame, text="削除", command=self.delete_memory, style="danger.TButton")
        delete_button.pack(side=LEFT, expand=True, fill=X)

    def load_memories_to_listbox(self):
        for item in self.memory_listbox.get_children():
            self.memory_listbox.delete(item)
        memories = self.memory_manager.get_all_memories()
        for key, value in memories.items():
            self.memory_listbox.insert("", "end", values=(key, value))

    def on_memory_select(self, event):
        selected_items = self.memory_listbox.selection()
        if not selected_items:
            return
        selected_item = selected_items[0]
        item = self.memory_listbox.item(selected_item)
        key, value = item['values']
        self.key_entry.delete(0, END)
        self.key_entry.insert(0, key)
        self.value_text.delete("1.0", END)
        self.value_text.insert("1.0", value)

    def save_memory(self):
        key = self.key_entry.get()
        value = self.value_text.get("1.0", END).strip()
        if not key:
            print("キーは必須です。")
            return
        self.memory_manager.add_or_update_memory(key, value)
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
        self.key_entry.delete(0, END)
        self.value_text.delete("1.0", END)
        self.memory_listbox.selection_remove(self.memory_listbox.selection())