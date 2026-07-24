-- Link async generation tasks (Seedance video, etc.) back to their submit-time
-- usage_logs row, so the poll handler can update authoritative token usage
-- (Seedance bills by completion_tokens, reported in the task-completion
-- response). Nullable: synchronous modalities (chat/image/tts/asr) leave it NULL.
ALTER TABLE usage_logs ADD COLUMN task_id TEXT;
