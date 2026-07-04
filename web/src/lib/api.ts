const API_BASE = '/api';

export interface Provider {
  name: string;
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
  | 'dashscope_asr'
  | 'dashscope_chat_image'
  | 'kling'
  | 'minimax_image';

export interface Route {
  id: string;
  base_url: string;
  ws_url?: string;
  model: string;
  provider: string;
  tags: string[];
  format: RouteFormat;
  enabled: boolean;
  tool_mode: 'native' | 'react_xml';
  path?: string;
}

export interface Tag {
  name: string;
  color: string;
  is_auto: boolean;
  route_priority: Record<string, number>;
}

export interface SmartRoutingConfig {
  enabled: boolean;
  cache_ttl_secs: number;
  cache_max_sessions: number;
  signal_tiers: Record<string, string>;
}

export interface AppConfig {
  port: number;
  host: string;
  providers: Record<string, Provider>;
  routes: Route[];
  tags: Tag[];
  current_tag: string;
  management_key: string;
  smart_routing: SmartRoutingConfig;
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
  caller_key_name?: string | null;
  input_tokens?: number | null;
  output_tokens?: number | null;
  latency_ms: number;
  cost: number;
  timestamp_ms: number;
}

export interface CallerKey {
  id: number;
  name: string;
  note: string;
  enabled: boolean;
  created_at: string;
  token?: string | null;
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

export interface ErrorEntry {
  timestamp: string;
  error_message: string;
  model: string;
}

export interface ProviderHealth {
  provider: string;
  total_requests: number;
  success_count: number;
  failure_count: number;
  success_rate: number;
  avg_latency_ms: number;
  total_input_tokens: number;
  total_output_tokens: number;
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
    credentials: 'include',
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

export async function getProviderHealth(): Promise<ProviderHealth[]> {
  const res = await fetch(`${API_BASE}/usage/provider-health`, { credentials: 'include' });
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

// ─── Fine-grained route CRUD ────────────────────────────────────────────

export async function createRoute(route: Route): Promise<{ index: number; id: string }> {
  const res = await fetch(`${API_BASE}/routes`, {
    method: 'POST',
    headers: jsonHeaders(),
    credentials: 'include',
    body: JSON.stringify(route),
  });
  await checkOk(res);
  return res.json();
}

export async function updateRoute(index: number, route: Route): Promise<Route> {
  const res = await fetch(`${API_BASE}/routes/${index}`, {
    method: 'PUT',
    headers: jsonHeaders(),
    credentials: 'include',
    body: JSON.stringify(route),
  });
  await checkOk(res);
  return res.json();
}

export async function patchRoute(index: number, patch: { enabled?: boolean }): Promise<Route> {
  const res = await fetch(`${API_BASE}/routes/${index}`, {
    method: 'PATCH',
    headers: jsonHeaders(),
    credentials: 'include',
    body: JSON.stringify(patch),
  });
  await checkOk(res);
  return res.json();
}

export async function deleteRoute(index: number): Promise<void> {
  const res = await fetch(`${API_BASE}/routes/${index}`, {
    method: 'DELETE',
    credentials: 'include',
  });
  await checkOk(res);
}

export async function moveRoute(index: number, direction: -1 | 1): Promise<Route> {
  const res = await fetch(`${API_BASE}/routes/${index}/move`, {
    method: 'POST',
    headers: jsonHeaders(),
    credentials: 'include',
    body: JSON.stringify({ direction }),
  });
  await checkOk(res);
  return res.json();
}

// ─── Fine-grained provider CRUD ─────────────────────────────────────────

export async function createProvider(id: string, provider: Provider): Promise<Provider> {
  const res = await fetch(`${API_BASE}/providers`, {
    method: 'POST',
    headers: jsonHeaders(),
    credentials: 'include',
    body: JSON.stringify({ id, provider }),
  });
  await checkOk(res);
  return res.json();
}

export async function updateProvider(id: string, provider: Provider): Promise<Provider> {
  const res = await fetch(`${API_BASE}/providers/${encodeURIComponent(id)}`, {
    method: 'PUT',
    headers: jsonHeaders(),
    credentials: 'include',
    body: JSON.stringify(provider),
  });
  await checkOk(res);
  return res.json();
}

export async function deleteProvider(id: string): Promise<void> {
  const res = await fetch(`${API_BASE}/providers/${encodeURIComponent(id)}`, {
    method: 'DELETE',
    credentials: 'include',
  });
  await checkOk(res);
}

// ─── Fine-grained tag CRUD ──────────────────────────────────────────────

export async function createTag(tag: Tag): Promise<Tag> {
  const res = await fetch(`${API_BASE}/tags`, {
    method: 'POST',
    headers: jsonHeaders(),
    credentials: 'include',
    body: JSON.stringify(tag),
  });
  await checkOk(res);
  return res.json();
}

export async function patchTag(
  name: string,
  patch: { route_priority?: Record<string, number>; color?: string }
): Promise<Tag> {
  const res = await fetch(`${API_BASE}/tags/${encodeURIComponent(name)}`, {
    method: 'PATCH',
    headers: jsonHeaders(),
    credentials: 'include',
    body: JSON.stringify(patch),
  });
  await checkOk(res);
  return res.json();
}

export async function deleteTag(name: string): Promise<void> {
  const res = await fetch(`${API_BASE}/tags/${encodeURIComponent(name)}`, {
    method: 'DELETE',
    credentials: 'include',
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

export async function testRouteByIndex(index: number, prompt?: string): Promise<TestResult> {
  const res = await fetch(`${API_BASE}/test/route`, {
    method: 'POST',
    headers: jsonHeaders(),
    credentials: 'include',
    body: JSON.stringify({ index, prompt }),
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
