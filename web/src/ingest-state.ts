export interface IngestOutcomeLike {
  status?: string | null
  error?: string | null
  notice?: string | null
  last_ingest_error?: string | null
  last_ingest_status?: string | null
}

export type UploadUiState = 'doing' | 'ok' | 'partial' | 'fail'

/** 列表里仍有后台入库任务时，面板重开后需要恢复空间级轮询。 */
export function isActiveIngest(source: IngestOutcomeLike): boolean {
  return source.status === 'pending' || source.status === 'parsing'
    || ['pending', 'parsing', 'processing', 'queued', 'running'].includes(source.last_ingest_status ?? '')
}

export function isTerminalIngest(source: IngestOutcomeLike): boolean {
  if (source.last_ingest_error || source.status === 'embedded' || source.status === 'failed') return true
  return source.status === 'chunked' && !!(source.error || source.notice)
}

/** pending/parsing 是正常的后台进行态，不得显示为处理失败。 */
export function ingestUploadState(source: IngestOutcomeLike): UploadUiState {
  if (source.last_ingest_error || source.status === 'failed') return 'fail'
  if (source.status === 'embedded') return 'ok'
  if (source.status === 'chunked' && isTerminalIngest(source)) return 'partial'
  return 'doing'
}
