const API_BASE = '/api';

export interface Provider {
  name: string;
  base_url: string;
  api_key: string;
  auth_type: 'bearer' | 'x_api_key' | 'x_goog_api_key';
}

export type RouteFormat =
  | 'openai'
  | 'anthropic'
  | 'openai_responses'
  | 'openai_images'
  | 'dashscope_image'
  | 'dashscope_video'
  | 'dashscope_tts'
  | 'kling'
  | 'minimax_image';

export const SUPPORTED_MODALITIES = [
  'chat',
  'vision',
  'image_generation',
  'video_generation',
  'tts',
  'asr',
  'embedding',
] as const;

export const MODALITY_LABELS: Record<string, string> = {
  chat: 'Chat',
  vision: 'Vision',
  image_generation: 'Image',
  video_generation: 'Video',
  tts: 'TTS',
  asr: 'ASR',
  embedding: 'Embedding',
};

export const FORMAT_MODALITIES: Record<RouteFormat, string> = {
  openai: 'chat',
  anthropic: 'chat',
  openai_responses: 'chat',
  openai_images: 'image_generation',
  dashscope_image: 'image_generation',
  minimax_image: 'image_generation',
  dashscope_video: 'video_generation',
  kling: 'video_generation',
  dashscope_tts: 'tts',
};

export const FORMAT_ENDPOINTS: Record<RouteFormat, string> = {
  openai: '/v1/chat/completions',
  anthropic: '/v1/messages',
  openai_responses: '/v1/responses',
  openai_images: '/v1/images/generations',
  dashscope_image: '/api/v1/services/aigc/text2image/image-synthesis',
  dashscope_video: '/api/v1/services/aigc/video-generation/video-synthesis',
  dashscope_tts: '/api/v1/services/aigc/text-to-speech/stream',
  kling: '/v1/videos/text2video',
  minimax_image: '/v1/image_generation',
};

export interface Route {
  endpoint: string;
  model: string;
  provider: string;
  tags: string[];
  format: RouteFormat;
  enabled: boolean;
  modality: string;
}

export interface Tag {
  name: string;
  color: string;
  is_auto: boolean;
}

export interface AppConfig {
  port: number;
  host: string;
  providers: Record<string, Provider>;
  routes: Route[];
  tags: Tag[];
  current_tag: string;
  management_key: string;
}

let managementKey: string | null = null;

export function setManagementKey(key: string) {
  managementKey = key;
}

function authHeaders(): Record<string, string> {
  const headers: Record<string, string> = {};
  if (managementKey) {
    headers['x-management-key'] = managementKey;
  }
  return headers;
}

export interface Status {
  current_tag: string;
  takeover: {
    active: boolean;
    proxy_url: string | null;
  };
  codex_takeover: {
    active: boolean;
    proxy_url: string | null;
  };
}

export interface RequestLog {
  request_model: string;
  tag: string;
  provider: string;
  target_model: string;
  modality: string;
  timestamp: string;
}

export async function getConfig(): Promise<AppConfig> {
  const res = await fetch(`${API_BASE}/config`);
  const config = await res.json();
  if (config.management_key) {
    setManagementKey(config.management_key);
  }
  return config;
}

export async function updateConfig(config: AppConfig): Promise<void> {
  await fetch(`${API_BASE}/config`, {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json', ...authHeaders() },
    body: JSON.stringify(config),
  });
}

export async function setCurrentTag(tag: string): Promise<void> {
  await fetch(`${API_BASE}/current-tag`, {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json', ...authHeaders() },
    body: JSON.stringify({ tag }),
  });
}

export async function getStatus(): Promise<Status> {
  const res = await fetch(`${API_BASE}/status`);
  return res.json();
}

export async function takeoverClaude(): Promise<{ proxy_url: string }> {
  const res = await fetch(`${API_BASE}/takeover/claude`, {
    method: 'POST',
    headers: authHeaders(),
  });
  return res.json();
}

export async function restoreClaude(): Promise<void> {
  await fetch(`${API_BASE}/takeover/claude`, {
    method: 'DELETE',
    headers: authHeaders(),
  });
}

export async function takeoverCodex(): Promise<{ proxy_url: string }> {
  const res = await fetch(`${API_BASE}/takeover/codex`, {
    method: 'POST',
    headers: authHeaders(),
  });
  return res.json();
}

export async function restoreCodex(): Promise<void> {
  await fetch(`${API_BASE}/takeover/codex`, {
    method: 'DELETE',
    headers: authHeaders(),
  });
}

export async function exportConfig(): Promise<AppConfig> {
  const res = await fetch(`${API_BASE}/config/export`, {
    method: 'POST',
    headers: { ...authHeaders() },
  });
  if (!res.ok) {
    const body = await res.json().catch(() => ({}));
    throw new Error(body.error || 'Export failed');
  }
  return res.json();
}

export async function importConfig(config: AppConfig): Promise<void> {
  const res = await fetch(`${API_BASE}/config/import`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', ...authHeaders() },
    body: JSON.stringify(config),
  });
  if (!res.ok) {
    const body = await res.json().catch(() => ({}));
    throw new Error(body.error || 'Import failed');
  }
}

export async function getLogs(): Promise<RequestLog[]> {
  const res = await fetch(`${API_BASE}/logs`);
  return res.json();
}

export interface TestResult {
  success: boolean;
  tag: string;
  provider: string;
  model: string;
  format: string;
  latency_ms: number;
  error: string | null;
  response: any | null;
}

export async function testRoute(tag: string, prompt?: string): Promise<TestResult> {
  const res = await fetch(`${API_BASE}/test`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', ...authHeaders() },
    body: JSON.stringify({ tag, prompt: prompt || 'Hi, reply with one word.' }),
  });
  return res.json();
}
