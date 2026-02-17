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


class SettingsWindow(tk.Toplevel):
    def __init__(self, parent, app):
        super().__init__(parent)
        self.app = app
        self.title("GameAssistant Advanced Settings")
        self.geometry("500x650")
        self.transient(parent)
        self.grab_set()

        self.create_widgets()

    def create_widgets(self):
        main_frame = ttk.Frame(self, padding=15)
        main_frame.pack(fill=BOTH, expand=True)

        self.notebook = ttk.Notebook(main_frame)
        self.notebook.pack(fill=BOTH, expand=True)

        self._create_engine_tab()
        self._create_twitch_tab()
        self._create_general_tab()

    def _create_engine_tab(self):
        tab = ttk.Frame(self.notebook, padding=15)
        self.notebook.add(tab, text=" Engines ")

        # TTS
        ttk.Label(tab, text="TTS Engine:", font=("TkDefaultFont", 10, "bold")).pack(anchor="w", pady=(0, 5))
        tts_frame = ttk.Frame(tab)
        tts_frame.pack(fill=X, pady=(0, 10))
        for engine in ["voicevox", "gemini", "style_bert_vits2"]:
            label_text = "VITS2" if engine == "style_bert_vits2" else engine.upper()
            ttk.Radiobutton(tts_frame, text=label_text, variable=self.app.state.tts_engine, value=engine, 
                           command=self.app.on_tts_engine_change).pack(side=LEFT, padx=5)

        # VITS2 Models
        self.app.vits2_config_frame = ttk.Frame(tab)
        ttk.Label(self.app.vits2_config_frame, text="VITS2 Model:").pack(anchor="w", pady=(5, 0))
        self.app.vits2_model_dropdown = ttk.Combobox(self.app.vits2_config_frame, state="readonly")
        self.app.vits2_model_dropdown.pack(fill=X, pady=2)
        self.app.vits2_model_dropdown.bind("<<ComboboxSelected>>", self.app.on_vits2_model_change)
        
        if self.app.state.tts_engine.get() == "style_bert_vits2":
            self.app.vits2_config_frame.pack(fill=X, pady=5)
            self.app.refresh_vits2_models()

        ttk.Separator(tab, orient="horizontal").pack(fill=X, pady=15)

        # ASR
        ttk.Label(tab, text="ASR Engine:", font=("TkDefaultFont", 10, "bold")).pack(anchor="w", pady=(0, 5))
        asr_frame = ttk.Frame(tab)
        asr_frame.pack(fill=X, pady=(0, 10))
        ttk.Radiobutton(asr_frame, text="LARGE (High Accuracy)", variable=self.app.state.asr_engine, value="large").pack(anchor="w", pady=2)
        ttk.Radiobutton(asr_frame, text="TINY (Lightweight)", variable=self.app.state.asr_engine, value="tiny").pack(anchor="w", pady=2)

        ttk.Separator(tab, orient="horizontal").pack(fill=X, pady=15)

        # Thinking Mode
        ttk.Checkbutton(tab, text="Disable Thinking Mode (Faster response)", 
                       variable=self.app.state.disable_thinking_mode, style="success-square-toggle",
                       command=lambda: self.app.state.save('disable_thinking_mode', self.app.state.disable_thinking_mode.get())).pack(anchor="w")

    def _create_twitch_tab(self):
        tab = ttk.Frame(self.notebook, padding=15)
        self.notebook.add(tab, text=" Twitch ")

        def _create_entry(label, var, key, show=None):
            f = ttk.Frame(tab)
            f.pack(fill=X, pady=5)
            ttk.Label(f, text=label, width=15).pack(side=LEFT)
            e = ttk.Entry(f, textvariable=var, show=show)
            e.pack(side=LEFT, fill=X, expand=True)
            e.bind("<FocusOut>", lambda ev: self.app.state.save(key, var.get()))

        _create_entry("Bot Username:", self.app.state.twitch_bot_username, 'twitch_bot_username')
        _create_entry("Bot ID:", self.app.state.twitch_bot_id, 'bot_id')
        _create_entry("Client ID:", self.app.state.twitch_client_id, 'twitch_client_id')
        _create_entry("Client Secret:", self.app.state.twitch_client_secret, 'twitch_client_secret', show="*")

        ttk.Separator(tab, orient="horizontal").pack(fill=X, pady=15)

        ttk.Label(tab, text="Authentication:", font=("TkDefaultFont", 10, "bold")).pack(anchor="w")
        auth_frame = ttk.Frame(tab)
        auth_frame.pack(fill=X, pady=10)
        ttk.Entry(auth_frame, textvariable=self.app.state.twitch_auth_code).pack(side=LEFT, fill=X, expand=True, padx=(0, 5))
        ttk.Button(auth_frame, text="Register Token", command=self.app.twitch_service.register_auth_code, style="success.TButton").pack(side=LEFT)
        
        ttk.Button(tab, text="Copy Auth URL to Clipboard", command=self.app.twitch_service.copy_auth_url, style="info.TButton").pack(fill=X, pady=5)

        ttk.Separator(tab, orient="horizontal").pack(fill=X, pady=15)

        self.app.twitch_connect_button = ttk.Button(tab, text="Connect Twitch", 
                                                   command=self.app.twitch_service.toggle_twitch_connection, 
                                                   style="primary.TButton")
        self.app.twitch_connect_button.pack(fill=X, pady=10)

    def _create_general_tab(self):
        tab = ttk.Frame(self.notebook, padding=15)
        self.notebook.add(tab, text=" General ")

        # Toggles
        def _create_toggle(text, var, key):
            cmd = lambda: self.app.state.save(key, var.get())
            ttk.Checkbutton(tab, text=text, variable=var, style="success-square-toggle", command=cmd).pack(anchor="w", pady=5)

        _create_toggle("Use Image (Vision)", self.app.state.use_image, 'use_image')
        _create_toggle("Private Mode", self.app.state.is_private, 'is_private')
        _create_toggle("Enable Auto-Commentary", self.app.state.enable_auto_commentary, 'enable_auto_commentary')
        
        # Auto-Commentary Intervals
        interval_frame = ttk.Frame(tab)
        interval_frame.pack(fill=X, pady=2, padx=(25, 0))
        ttk.Label(interval_frame, text="Interval (s) Min:").pack(side=LEFT)
        e_min = ttk.Entry(interval_frame, textvariable=self.app.state.auto_commentary_min, width=10)
        e_min.pack(side=LEFT, padx=5)
        e_min.bind("<FocusOut>", lambda e: self.app.state.save('auto_commentary_min', self.app.state.auto_commentary_min.get()))
        
        ttk.Label(interval_frame, text="Max:").pack(side=LEFT)
        e_max = ttk.Entry(interval_frame, textvariable=self.app.state.auto_commentary_max, width=10)
        e_max.pack(side=LEFT, padx=5)
        e_max.bind("<FocusOut>", lambda e: self.app.state.save('auto_commentary_max', self.app.state.auto_commentary_max.get()))

        # Avoid Overlap Settings
        avoid_frame = ttk.Frame(tab)
        avoid_frame.pack(fill=X, pady=2, padx=(25, 0))
        ttk.Checkbutton(avoid_frame, text="Avoid Overlap", variable=self.app.state.auto_commentary_avoid_overlap, 
                       style="success-square-toggle",
                       command=lambda: self.app.state.save('auto_commentary_avoid_overlap', self.app.state.auto_commentary_avoid_overlap.get())).pack(side=LEFT)
        
        ttk.Label(avoid_frame, text=" Wait (s):").pack(side=LEFT, padx=(10, 0))
        e_avoid = ttk.Entry(avoid_frame, textvariable=self.app.state.auto_commentary_avoid_duration, width=8)
        e_avoid.pack(side=LEFT, padx=5)
        e_avoid.bind("<FocusOut>", lambda e: self.app.state.save('auto_commentary_avoid_duration', self.app.state.auto_commentary_avoid_duration.get()))

        _create_toggle("Show Response in New Window", self.app.state.show_response_in_new_window, 'show_response_in_new_window')
        _create_toggle("Create Blog Post after session", self.app.state.create_blog_post, 'create_blog_post')
        
        # Blog Thinking Toggle
        blog_thinking_frame = ttk.Frame(tab)
        blog_thinking_frame.pack(fill=X, pady=2, padx=(25, 0))
        ttk.Checkbutton(blog_thinking_frame, text="Use Thinking for Blog", variable=self.app.state.blog_use_thinking, 
                       style="success-square-toggle",
                       command=lambda: self.app.state.save('blog_use_thinking', self.app.state.blog_use_thinking.get())).pack(side=LEFT)

        ttk.Separator(tab, orient="horizontal").pack(fill=X, pady=15)

        # Entries
        f1 = ttk.Frame(tab)
        f1.pack(fill=X, pady=5)
        ttk.Label(f1, text="User Name:", width=20).pack(side=LEFT)
        e1 = ttk.Entry(f1, textvariable=self.app.state.user_name)
        e1.pack(side=LEFT, fill=X, expand=True)
        e1.bind("<FocusOut>", lambda e: self.app.state.save('user_name', self.app.state.user_name.get()))

        f2 = ttk.Frame(tab)
        f2.pack(fill=X, pady=5)
        ttk.Label(f2, text="Display Duration (ms):", width=20).pack(side=LEFT)
        e2 = ttk.Entry(f2, textvariable=self.app.state.response_display_duration)
        e2.pack(side=LEFT, fill=X, expand=True)
        e2.bind("<FocusOut>", lambda e: self.app.state.save('response_display_duration', self.app.state.response_display_duration.get()))


class MemoryWindow(tk.Toplevel):
    def __init__(self, parent, app, memory_manager, gemini_service):
        super().__init__(parent)
        self.app = app
        self.memory_manager = memory_manager
        self.gemini_service = gemini_service
        self.title("Memory Management")
        self.geometry("1040x650") # Width increased by 40px, height slightly increased for comfort
        self.minsize(900, 500)

        self.create_widgets()
        self.load_memories_to_listbox()

    def create_widgets(self):
        # Background consistent with main app
        main_frame = ttk.Frame(self, padding=15)
        main_frame.pack(fill=BOTH, expand=True)

        # --- Left Side: List Area ---
        list_container = ttk.Frame(main_frame)
        list_container.pack(side=LEFT, fill=BOTH, expand=True, padx=(0, 15))

        # Treeview with Scrollbar
        tree_frame = ttk.Frame(list_container)
        tree_frame.pack(fill=BOTH, expand=True)

        columns = ("timestamp", "key", "type", "user", "comment")
        self.memory_listbox = ttk.Treeview(tree_frame, columns=columns, show="headings", selectmode="extended")
        
        # Scrollbar setup
        scrollbar = ttk.Scrollbar(tree_frame, orient=VERTICAL, command=self.memory_listbox.yview)
        self.memory_listbox.configure(yscrollcommand=scrollbar.set)
        
        self.memory_listbox.heading("timestamp", text="Timestamp", command=lambda: self.sort_column("timestamp", False))
        self.memory_listbox.heading("key", text="Key", command=lambda: self.sort_column("key", False))
        self.memory_listbox.heading("type", text="Type", command=lambda: self.sort_column("type", False))
        self.memory_listbox.heading("user", text="User", command=lambda: self.sort_column("user", False))
        self.memory_listbox.heading("comment", text="Comment", command=lambda: self.sort_column("comment", False))

        self.memory_listbox.column("timestamp", width=150, anchor='w')
        self.memory_listbox.column("key", width=100, anchor='w')
        self.memory_listbox.column("type", width=80, anchor='w')
        self.memory_listbox.column("user", width=80, anchor='w')
        self.memory_listbox.column("comment", width=350, anchor='w')

        self.memory_listbox.pack(side=LEFT, fill=BOTH, expand=True)
        scrollbar.pack(side=RIGHT, fill=Y)
        
        self.memory_listbox.bind("<<TreeviewSelect>>", self.on_memory_select)

        # --- Right Side: Details Area ---
        right_frame = ttk.Labelframe(main_frame, text=" MEMORY DETAILS ", padding=15, style="Card.TLabelframe")
        right_frame.pack(side=RIGHT, fill=Y, width=320)
        right_frame.pack_propagate(False)

        # Key (Readonly)
        ttk.Label(right_frame, text="Key:", font=("TkDefaultFont", 9, "bold")).pack(anchor="w")
        self.key_entry = ttk.Entry(right_frame, state='readonly', font=("Consolas", 9))
        self.key_entry.pack(fill=X, pady=(2, 10))

        # Metadata Row 1
        meta_frame = ttk.Frame(right_frame)
        meta_frame.pack(fill=X, pady=(0, 10))
        
        type_sub = ttk.Frame(meta_frame)
        type_sub.pack(side=LEFT, fill=X, expand=True, padx=(0, 5))
        ttk.Label(type_sub, text="Type:", font=("TkDefaultFont", 9, "bold")).pack(anchor="w")
        self.type_entry = ttk.Entry(type_sub)
        self.type_entry.pack(fill=X, pady=2)

        user_sub = ttk.Frame(meta_frame)
        user_sub.pack(side=LEFT, fill=X, expand=True)
        ttk.Label(user_sub, text="User:", font=("TkDefaultFont", 9, "bold")).pack(anchor="w")
        self.user_entry = ttk.Entry(user_sub)
        self.user_entry.pack(fill=X, pady=2)

        # Timestamp (Label only)
        ttk.Label(right_frame, text="Timestamp:", font=("TkDefaultFont", 9, "bold")).pack(anchor="w")
        self.timestamp_label = ttk.Label(right_frame, text="-", foreground="#64748b")
        self.timestamp_label.pack(anchor="w", pady=(2, 10))

        # Comment Text
        ttk.Label(right_frame, text="Content:", font=("TkDefaultFont", 9, "bold")).pack(anchor="w")
        self.comment_text = tk.Text(right_frame, height=12, font=("Arial", 10), wrap=tk.WORD, 
                                   bg="#1a1a3a", fg="white", insertbackground="white", 
                                   padx=5, pady=5, borderwidth=1, relief="flat")
        self.comment_text.pack(fill=BOTH, expand=True, pady=(2, 15))

        # --- Button Actions ---
        btn_container = ttk.Frame(right_frame)
        btn_container.pack(fill=X, side='bottom')

        # Row 1: Save & Delete
        row1 = ttk.Frame(btn_container)
        row1.pack(fill=X, pady=(0, 8))
        
        save_btn = ttk.Button(row1, text="💾 Save Changes", command=self.save_memory, style="success.TButton")
        save_btn.pack(side=LEFT, expand=True, fill=X, padx=(0, 4))

        del_btn = ttk.Button(row1, text="🗑️ Delete", command=self.delete_memory, style="danger.TButton")
        del_btn.pack(side=LEFT, expand=True, fill=X)

        # Row 2: Blog Generation (Primary Action)
        self.blog_btn = ttk.Button(btn_container, text="📝 Generate Blog from Selected", 
                                  command=self.generate_blog_from_selection, style="info.TButton")
        self.blog_btn.pack(fill=X)

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

        deleted_count = 0
        for item_id in selected_items:
            item = self.memory_listbox.item(item_id)
            values = item['values']
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
