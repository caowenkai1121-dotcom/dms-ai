/**
 * KB 流式问答（`kb-ask-stream v1`）的客户端侧纯函数：SSE 帧解析 + 事件分派。
 * POST 流不能用 EventSource（只支持 GET），所以 fetch + ReadableStream 按字节读、自己拼帧。
 *
 * 协议（服务端 `kb_api.rs`「SSE 流式问答」段头注是唯一事实源，这里只是镜像）：
 * - `event: meta`  {"trace_id","citations":[…],"searched_docs":N|null,"conv_id"?,"space_id"?}
 * - `event: delta` {"text":"…"}（正文增量预览，未过口径后处理；done 时必须整体替换）
 * - `event: done`  {"answer":{Answer}}（与 /api/kb/ask 同步端点同 wire）
 * - `event: error` {"message":"固定友好文案"}
 * 帧 = 若干 `field: value` 行 + 空行收尾；`:` 开头是注释行（keep-alive 心跳），忽略。
 */

export interface SseEvent {
  /** `event:` 字段；缺省按 SSE 规范是 'message'（本协议恒带事件名） */
  event: string
  /** 多行 data 按规范以 '\n' 拼接（本协议 data 恒单行 JSON） */
  data: string
}

/** 增量喂文本、吐出完整帧；跨 chunk 的半行留在内部缓冲。 */
export class SseParser {
  private buf = ''
  private event = ''
  private dataLines: string[] = []

  feed(chunk: string): SseEvent[] {
    this.buf += chunk
    const out: SseEvent[] = []
    let idx: number
    // 只处理以 \n 结尾的完整行；残余半行留缓冲（\r\n 的 \r 在行尾剥掉）
    while ((idx = this.buf.indexOf('\n')) >= 0) {
      const line = this.buf.slice(0, idx).replace(/\r$/, '')
      this.buf = this.buf.slice(idx + 1)
      if (line === '') {
        const ev = this.dispatch()
        if (ev) out.push(ev)
        continue
      }
      this.field(line)
    }
    return out
  }

  /** 流收尾：残余半行按一行处理，未配空行的最后一帧也吐出（RFC 6797 的 dispatch 语义）。 */
  end(): SseEvent[] {
    const out: SseEvent[] = []
    if (this.buf) {
      const line = this.buf.replace(/\r$/, '')
      this.buf = ''
      if (line === '') {
        const ev = this.dispatch()
        if (ev) out.push(ev)
      } else {
        this.field(line)
      }
    }
    const ev = this.dispatch()
    if (ev) out.push(ev)
    return out
  }

  private field(line: string) {
    if (line.startsWith(':')) return // 注释行（keep-alive `: ping`）
    const colon = line.indexOf(':')
    const name = colon < 0 ? line : line.slice(0, colon)
    // 冒号后剥**至多一个**前导空格（规范原文：a single leading space is removed）
    let value = colon < 0 ? '' : line.slice(colon + 1)
    if (value.startsWith(' ')) value = value.slice(1)
    if (name === 'event') this.event = value
    else if (name === 'data') this.dataLines.push(value)
    // id/retry 与本协议无关，忽略
  }

  private dispatch(): SseEvent | null {
    if (!this.dataLines.length) {
      this.event = ''
      return null // 只有 event:/注释 的帧（如心跳注释行后的空行）不产出事件
    }
    const ev: SseEvent = { event: this.event || 'message', data: this.dataLines.join('\n') }
    this.event = ''
    this.dataLines = []
    return ev
  }
}

/** meta 事件的载荷形状（客户端只消费这几键；多余键忽略，前向兼容） */
export interface KbStreamMeta {
  trace_id?: string
  citations?: unknown[]
  searched_docs?: number | null
  conv_id?: number
  space_id?: string | null
}

/** 解析一帧的 data JSON：坏 JSON 返回 null（调用方跳过该帧，不炸整条流） */
export function parseEventData(ev: SseEvent): Record<string, unknown> | null {
  try {
    const v: unknown = JSON.parse(ev.data)
    return v && typeof v === 'object' && !Array.isArray(v) ? (v as Record<string, unknown>) : null
  } catch {
    return null
  }
}
