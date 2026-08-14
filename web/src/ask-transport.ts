/**
 * 流式优先，但只允许在 fetch 尚未拿到任何 HTTP 响应时回退同步端点。
 * 一旦拿到响应，请求可能已经在服务端执行；此后断流必须报错，不能自动重放。
 */
export async function runAskTransport(
  post: (url: string) => Promise<Response>,
  consumeStream: (response: Response) => Promise<void>,
  handleSync: (response: Response) => Promise<void>,
  signal: AbortSignal,
): Promise<void> {
  let response: Response
  try {
    response = await post('/api/ask/stream')
  } catch (error) {
    if (signal.aborted) throw error
    await handleSync(await post('/api/ask'))
    return
  }

  const contentType = response.headers.get('content-type') ?? ''
  if (response.ok && contentType.includes('text/event-stream')) {
    await consumeStream(response)
  } else {
    await handleSync(response)
  }
}

/** 同一登录态的并发/迟到 401 只执行一次过期清理。 */
export function createSessionExpiryGuard(handler: () => Promise<void>): (sessionKey: string) => Promise<void> {
  let handledKey: string | undefined
  let inFlight: Promise<void> | null = null
  const expire = (sessionKey: string): Promise<void> => {
    if (sessionKey === handledKey) return inFlight ?? Promise.resolve()
    if (inFlight) return inFlight.then(() => expire(sessionKey), () => expire(sessionKey))
    handledKey = sessionKey
    const task = handler().finally(() => {
      if (inFlight === task) inFlight = null
    })
    inFlight = task
    return task
  }
  return expire
}
