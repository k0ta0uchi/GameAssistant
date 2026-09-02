// TypeScript 型定義

export interface SystemStatus {
  asr: boolean;
  gemini: boolean;
  tts: boolean;
  twitch: boolean;
  session: boolean;
}

export interface LogEntry {
  type: 'log';
  timestamp: string;
  level: 'DEBUG' | 'INFO' | 'WARNING' | 'ERROR' | 'CRITICAL';
  logger: string;
  message: string;
}

export interface AsrEvent {
  type: 'asr';
  text: string;
  is_final: boolean;
}

export interface AsrEntry {
  id: string;
  text: string;
  timestamp: string;
  isDiscord?: boolean;
  isPrompt?: boolean;
}

export interface LevelMeterEvent {
  type: 'level_meter';
  level: number;
}

export interface GeminiResponseEvent {
  type: 'gemini_response';
  text: string;
}

export interface ResourceInfo {
  used: number; // MB
  total: number; // MB
  percent: number; // %
}

export interface ResourceStatusEvent {
  type: 'resource_status';
  vram: ResourceInfo;
  ram: ResourceInfo;
}

export interface CommentaryTimerEvent {
  type: 'commentary_timer';
  progress: number;
  remaining: number;
}

export interface StatusEvent {
  type: 'status';
  status: SystemStatus;
}

export interface LogHistoryEvent {
  type: 'log_history';
  logs: LogEntry[];
}

export type WsMessage =
  | LogEntry
  | LogHistoryEvent
  | AsrEvent
  | LevelMeterEvent
  | GeminiResponseEvent
  | ResourceStatusEvent
  | CommentaryTimerEvent
  | StatusEvent;

export interface SkillItem {
  id: string;
  name: string;
  description: string;
  guidelines?: string;
  file_path?: string;
}

export interface SkillsResponse {
  skills: SkillItem[];
  enabled_skills: string[];
  master_enabled: boolean;
}

export interface MemoryItem {
  id: string;
  key?: string;
  content: string;
  source?: string;
  user?: string;
  type?: string;
  timestamp?: string;
  display_ts?: string;
}

export interface PromptItem {
  id: string;
  title: string;
  category: 'Character' | 'Commentary' | 'Blog' | 'Memory' | 'Voice';
  icon: string;
  description: string;
  default: string;
  value: string;
  is_modified: boolean;
}

export interface ModelStatus {
  id: string;
  name: string;
  description: string;
  hf_repo: string;
  category: 'ASR' | 'Embedding' | 'LLM' | 'Other';
  required: boolean;
  estimated_size_bytes: number;
  is_installed: boolean;
  actual_size_bytes: number;
  local_path: string;
}

export interface DownloadProgressEvent {
  model_id: string;
  current_bytes: number;
  total_bytes: number;
  speed_mbps: number;
  percent: number;
  status: 'downloading' | 'completed' | 'error' | 'cancelled';
  error_message?: string;
}
