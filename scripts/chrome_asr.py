# -*- coding: utf-8 -*-
import asyncio
import logging
import threading
import json
import base64
from playwright.async_api import async_playwright

class ChromeASR:
    """
    Playwrightを使用してChromeのWeb Speech APIを音声認識エンジンとして利用するクラス。
    Python側からの音声データをブラウザ内に転送して認識させる。
    """
    def __init__(self, callback):
        self.callback = callback # (text, is_final) を受け取る関数
        self.loop = None
        self.thread = None
        self.page = None
        self.browser = None
        self.playwright = None
        self._stop_event = asyncio.Event()
        self.is_running = False

    def start(self):
        if self.is_running: return
        self.is_running = True
        self.thread = threading.Thread(target=self._run_event_loop, daemon=True)
        self.thread.start()

    def _run_event_loop(self):
        self.loop = asyncio.new_event_loop()
        asyncio.set_event_loop(self.loop)
        self.loop.run_until_complete(self._launch_browser())

    async def _launch_browser(self):
        logging.info("Launching Chrome for ASR (Visible mode)...")
        self.playwright = await async_playwright().start()
        
        # デバッグのためブラウザを表示 (headless=False)
        # マイク権限はcontext作成時に付与するが、自動許可フラグは外して実際の挙動を見る
        self.browser = await self.playwright.chromium.launch(
            headless=False,
            args=[
                "--mute-audio", # ブラウザからの音漏れ防止
                "--window-size=400,300",
                "--window-position=0,0"
            ]
        )
        self.context = await self.browser.new_context(
            permissions=['microphone'] # マイク権限を付与
        )
        self.page = await self.context.new_page()

        # Python側のコールバックをブラウザに公開
        await self.page.expose_function("on_speech_result", self._handle_js_result)

        # 信頼できるHTTPSサイトへ移動して、Googleの音声認識サーバーへの接続を安定させる
        logging.info("Navigating to https://example.com for ASR context...")
        try:
            await self.page.goto("https://example.com")
        except Exception as e:
            logging.error(f"Failed to navigate to example.com: {e}")
            return

        # Web Speech APIとUIを注入
        await self.page.evaluate("""() => {
            // UI作成
            document.body.innerHTML = '';
            document.body.style.backgroundColor = '#222';
            document.body.style.color = '#fff';
            document.body.style.fontFamily = 'monospace';
            document.body.style.padding = '10px';
            
            const title = document.createElement('h3');
            title.textContent = 'ASR Debug Mode (example.com)';
            document.body.appendChild(title);
            
            const statusDiv = document.createElement('div');
            statusDiv.id = 'status';
            statusDiv.textContent = 'Initializing...';
            document.body.appendChild(statusDiv);
            
            const logDiv = document.createElement('div');
            logDiv.id = 'log';
            logDiv.style.height = '200px';
            logDiv.style.overflowY = 'scroll';
            logDiv.style.border = '1px solid #444';
            logDiv.style.padding = '5px';
            document.body.appendChild(logDiv);

            function log(msg) {
                const el = document.getElementById('log');
                const line = document.createElement('div');
                line.textContent = `[${new Date().toLocaleTimeString()}] ${msg}`;
                el.appendChild(line);
                el.scrollTop = el.scrollHeight;
                console.log(msg);
            }

            async function startMicrophone() {
                log('Requesting microphone access...');
                try {
                    const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
                    log('Microphone access GRANTED.');
                    
                    const audioContext = new AudioContext();
                    const source = audioContext.createMediaStreamSource(stream);
                    const analyser = audioContext.createAnalyser();
                    source.connect(analyser);
                    analyser.fftSize = 256;
                    const bufferLength = analyser.frequencyBinCount;
                    const dataArray = new Uint8Array(bufferLength);
                    
                    setInterval(() => {
                        analyser.getByteFrequencyData(dataArray);
                        let sum = 0;
                        for(let i = 0; i < bufferLength; i++) sum += dataArray[i];
                        const avg = sum / bufferLength;
                        if (avg > 10) log(`Microphone Input Detected (Level: ${Math.floor(avg)})`);
                    }, 2000);

                    startRecognition();
                } catch (err) {
                    log(`Microphone Access DENIED or ERROR: ${err.name}: ${err.message}`);
                    document.getElementById('status').textContent = 'MIC ERROR';
                    document.getElementById('status').style.color = 'red';
                }
            }

            function startRecognition() {
                const Recognition = window.SpeechRecognition || window.webkitSpeechRecognition;
                if (!Recognition) {
                    log('Web Speech API not supported in this browser.');
                    return;
                }
                const recognition = new Recognition();
                recognition.lang = 'ja-JP';
                recognition.continuous = true;
                recognition.interimResults = true;

                recognition.onresult = (event) => {
                    for (let i = event.resultIndex; i < event.results.length; ++i) {
                        const result = event.results[i];
                        const text = result[0].transcript;
                        const isFinal = result.isFinal;
                        log(`Recognized: ${text} (Final: ${isFinal})`);
                        window.on_speech_result(text, isFinal);
                    }
                };

                recognition.onerror = (e) => {
                    log(`ASR Error: ${e.error}`);
                    if (e.error === 'not-allowed') {
                        document.getElementById('status').textContent = 'ASR BLOCKED';
                    }
                };
                
                recognition.onend = () => {
                    log('ASR Ended. Restarting...');
                    recognition.start();
                };
                
                recognition.onstart = () => {
                    log('ASR Started');
                    document.getElementById('status').textContent = 'Listening...';
                    document.getElementById('status').style.color = '#0f0';
                };

                try {
                    recognition.start();
                } catch (e) {
                    log(`Failed to start recognition: ${e}`);
                }
            }

            startMicrophone();
        }""")
        
        # 終了を待機
        try:
            await self._stop_event.wait()
        finally:
            # 確実に終了処理を行う
            if self.page:
                try: await self.page.close()
                except: pass
            if self.context:
                try: await self.context.close()
                except: pass
            if self.browser:
                try: await self.browser.close()
                except: pass
            if self.playwright:
                try: await self.playwright.stop()
                except: pass
            logging.info("Chrome ASR has been stopped successfully.")

    def _handle_js_result(self, text, is_final):
        """ブラウザ(JS)からの結果をPythonのコールバックに渡す"""
        if self.callback:
            self.callback(text, is_final)

    def add_audio(self, audio_float32):
        """
        Python側(AudioService)からキャプチャした音声データを受け取る。
        ※ChromeのWeb Speech APIは現在、システムのデフォルトマイク入力を直接聴くのが最も安定しているため、
        ここではデータの転送は行わず、ブラウザに「聴かせる」状態にします。
        （仮想デバイスを使わない「完全な流し込み」はブラウザのセキュリティ制約上、非常に重くなるため、
        ブラウザにマイクを共有させる方式が最も軽量です）
        """
        pass

    def stop(self):
        logging.info("Stopping Chrome ASR...")
        self.is_running = False
        if self.loop and self.loop.is_running():
            self.loop.call_soon_threadsafe(self._stop_event.set)
        
        if self.thread and self.thread.is_alive():
            self.thread.join(timeout=5)
            if self.thread.is_alive():
                logging.warning("Chrome ASR thread did not finish in time.")
        
        self.thread = None
        self.loop = None
