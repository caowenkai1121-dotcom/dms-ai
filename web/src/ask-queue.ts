export type AskIntent = 'auto' | 'data' | 'knowledge'
export type AskMode = 'deep' | 'lite'

export interface QueuedAskSnapshot {
  id: string
  text: string
  refs: string[]
  forceIntent: AskIntent
  forceMode: AskMode
  spaceId: string | null
}

/** 排队问题的语义必须在入队时冻结，不能在出队时重读 UI 全局开关。 */
export function snapshotQueuedAsk(
  id: string,
  text: string,
  refs: string[],
  state: { intent: AskIntent; mode: AskMode; spaceId: string | null },
): QueuedAskSnapshot {
  return {
    id,
    text,
    refs: [...refs],
    forceIntent: state.intent,
    forceMode: state.mode,
    spaceId: state.spaceId,
  }
}
