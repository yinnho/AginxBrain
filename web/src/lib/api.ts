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
  setup_required: boolean;
}

export interface RequestLog {
  request_model: string;
  tag: string;
  provider: string;
  target_model: string;
  modality: string;
  timestamp: string;
}

export interface CallerKey {
  id: number;
  name: string;
  note: string;
  enabled: boolean;
  created_at: string;
}

export interface CreateCallerKeyResponse extends CallerKey {
  token: string;
}

export interface CostRate {
  id: number;
  provider: string;
  model: string;
  input_price_per_1k: number;
  output_price_per_1k: number;
}

export interface DailyUsage {
  day: string;
  caller_key_id: number | null;
  request_count: number;
  input_tokens: number;
  output_tokens: number;
  estimated_cost: number;
}

export interface MonthlyUsage {
  month: string;
  caller_key_id: number | null;
  request_count: number;
  input_tokens: number;
  output_tokens: number;
  estimated_cost: number;
}

export interface UsageSummary {
  caller_key_id: number | null;
  request_count: number;
  input_tokens: number;
  output_tokens: number;
  estimated_cost: number;
}

function jsonHeaders(): Record<string, string> {
  return { 'Content-Type': 'application/json' };
}

async function checkOk(res: Response): Promise<void> {
  if (!res.ok) {
    const body = await res.json().catch(() => ({}));
    throw new Error(body.error || `Request failed: ${res.status}`);
  }
}

// ─── Admin auth ─────────────────────────────────────────────────────────

export async function adminSetup(username: string, password: string): Promise<void> {
  const res = await fetch(`${API_BASE}/admin/setup`, {
    method: 'POST',
    headers: jsonHeaders(),
    body: JSON.stringify({ username, password }),
  });
  await checkOk(res);
}

export async function adminLogin(username: string, password: string): Promise<void> {
  const res = await fetch(`${API_BASE}/admin/login`, {
    method: 'POST',
    headers: jsonHeaders(),
    body: JSON.stringify({ username, password }),
    credentials: 'include',
  });
  await checkOk(res);
}

export async function adminLogout(): Promise<void> {
  await fetch(`${API_BASE}/admin/logout`, {
    method: 'POST',
    credentials: 'include',
  });
}

export async function getMe(): Promise<{ username: string }> {
  const res = await fetch(`${API_BASE}/admin/me`, { credentials: 'include' });
  await checkOk(res);
  return res.json();
}

// ─── Caller keys ────────────────────────────────────────────────────────

export async function listKeys(): Promise<CallerKey[]> {
  const res = await fetch(`${API_BASE}/keys`, { credentials: 'include' });
  await checkOk(res);
  return res.json();
}

export async function createKey(name: string, note?: string): Promise<CreateCallerKeyResponse> {
  const res = await fetch(`${API_BASE}/keys`, {
    method: 'POST',
    headers: jsonHeaders(),
    credentials: 'include',
    body: JSON.stringify({ name, note }),
  });
  await checkOk(res);
  return res.json();
}

export async function updateKey(key: CallerKey): Promise<void> {
  const res = await fetch(`${API_BASE}/keys/${key.id}`, {
    method: 'PUT',
    headers: jsonHeaders(),
    credentials: 'include',
    body: JSON.stringify(key),
  });
  await checkOk(res);
}

export async function deleteKey(id: number): Promise<void> {
  const res = await fetch(`${API_BASE}/keys/${id}`, {
    method: 'DELETE',
    credentials: 'include',
  });
  await checkOk(res);
}

// ─── Cost rates ─────────────────────────────────────────────────────────

export async function listCostRates(): Promise<CostRate[]> {
  const res = await fetch(`${API_BASE}/cost-rates`, { credentials: 'include' });
  await checkOk(res);
  return res.json();
}

export async function setCostRate(rate: Omit<CostRate, 'id'>): Promise<CostRate> {
  const res = await fetch(`${API_BASE}/cost-rates`, {
    method: 'POST',
    headers: jsonHeaders(),
    credentials: 'include',
    body: JSON.stringify(rate),
  });
  await checkOk(res);
  return res.json();
}

export async function deleteCostRate(id: number): Promise<void> {
  const res = await fetch(`${API_BASE}/cost-rates/${id}`, {
    method: 'DELETE',
    credentials: 'include',
  });
  await checkOk(res);
}

// ─── Usage ──────────────────────────────────────────────────────────────

export async function getDailyUsage(
  from: string,
  to: string,
  keyId?: number | null
): Promise<DailyUsage[]> {
  const params = new URLSearchParams({ from, to });
  if (keyId != null) params.set('key_id', String(keyId));
  const res = await fetch(`${API_BASE}/usage/daily?${params}`, { credentials: 'include' });
  await checkOk(res);
  return res.json();
}

export async function getMonthlyUsage(
  year: number,
  month: number,
  keyId?: number | null
): Promise<MonthlyUsage[]> {
  const params = new URLSearchParams({ year: String(year), month: String(month) });
  if (keyId != null) params.set('key_id', String(keyId));
  const res = await fetch(`${API_BASE}/usage/monthly?${params}`, { credentials: 'include' });
  await checkOk(res);
  return res.json();
}

export async function getUsageSummary(): Promise<UsageSummary[]> {
  const res = await fetch(`${API_BASE}/usage/summary`, { credentials: 'include' });
  await checkOk(res);
  return res.json();
}

// ─── Config / status / logs ─────────────────────────────────────────────

export async function getConfig(): Promise<AppConfig> {
  const res = await fetch(`${API_BASE}/config`, { credentials: 'include' });
  await checkOk(res);
  return res.json();
}

export async function updateConfig(config: AppConfig): Promise<void> {
  const res = await fetch(`${API_BASE}/config`, {
    method: 'PUT',
    headers: jsonHeaders(),
    credentials: 'include',
    body: JSON.stringify(config),
  });
  await checkOk(res);
}

export async function setCurrentTag(tag: string): Promise<void> {
  const res = await fetch(`${API_BASE}/current-tag`, {
    method: 'PUT',
    headers: jsonHeaders(),
    credentials: 'include',
    body: JSON.stringify({ tag }),
  });
  await checkOk(res);
}

export async function getStatus(): Promise<Status> {
  const res = await fetch(`${API_BASE}/status`, { credentials: 'include' });
  await checkOk(res);
  return res.json();
}

export async function takeoverClaude(): Promise<{ proxy_url: string }> {
  const res = await fetch(`${API_BASE}/takeover/claude`, {
    method: 'POST',
    credentials: 'include',
  });
  await checkOk(res);
  return res.json();
}

export async function restoreClaude(): Promise<void> {
  const res = await fetch(`${API_BASE}/takeover/claude`, {
    method: 'DELETE',
    credentials: 'include',
  });
  await checkOk(res);
}

export async function takeoverCodex(): Promise<{ proxy_url: string }> {
  const res = await fetch(`${API_BASE}/takeover/codex`, {
    method: 'POST',
    credentials: 'include',
  });
  await checkOk(res);
  return res.json();
}

export async function restoreCodex(): Promise<void> {
  const res = await fetch(`${API_BASE}/takeover/codex`, {
    method: 'DELETE',
    credentials: 'include',
  });
  await checkOk(res);
}

export async function exportConfig(): Promise<AppConfig> {
  const res = await fetch(`${API_BASE}/config/export`, {
    method: 'POST',
    credentials: 'include',
  });
  await checkOk(res);
  return res.json();
}

export async function importConfig(config: AppConfig): Promise<void> {
  const res = await fetch(`${API_BASE}/config/import`, {
    method: 'POST',
    headers: jsonHeaders(),
    credentials: 'include',
    body: JSON.stringify(config),
  });
  await checkOk(res);
}

export async function getLogs(): Promise<RequestLog[]> {
  const res = await fetch(`${API_BASE}/logs`, { credentials: 'include' });
  await checkOk(res);
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
    headers: jsonHeaders(),
    credentials: 'include',
    body: JSON.stringify({ tag, prompt }),
  });
  await checkOk(res);
  return res.json();
}

export interface GenerateImageRequest {
  tag?: string;
  prompt: string;
  size?: string;
  n?: number;
  extra?: Record<string, any>;
}

export interface GeneratedImage {
  url: string | null;
  base64: string;
}

export interface GenerateImageResponse {
  success: boolean;
  tag: string;
  provider: string;
  model: string;
  format: string;
  images: GeneratedImage[];
  latency_ms: number;
  error?: string;
}

export async function generateImage(req: GenerateImageRequest): Promise<GenerateImageResponse> {
  const res = await fetch(`${API_BASE}/brain/generate/image`, {
    method: 'POST',
    headers: jsonHeaders(),
    credentials: 'include',
    body: JSON.stringify(req),
  });
  await checkOk(res);
  return res.json();
}
