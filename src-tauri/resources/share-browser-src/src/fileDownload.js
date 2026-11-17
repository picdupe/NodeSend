import { t } from './i18n'

const CHUNK_SIZE = 16 * 1024 * 1024
const PARALLEL_CHUNKS = 4

async function remoteSize(fileUrl) {
  const response = await fetch(fileUrl, { method: 'HEAD', cache: 'no-store' })
  if (!response.ok) throw new Error(`${t('fileInfoFailed')}${response.status}）`)
  return Number(response.headers.get('Content-Length') || 0)
}

export async function downloadFileToHandle(fileHandle, fileUrl, onProgress) {
  const size = await remoteSize(fileUrl)
  const existing = await fileHandle.getFile()
  let offset = Math.min(existing.size, size)
  if (existing.size > size) {
    const writable = await fileHandle.createWritable({ keepExistingData: true })
    try {
      await writable.truncate(size)
      await writable.close()
    } catch (err) {
      try { await writable.abort() } catch {}
      throw err
    }
  }
  if (offset === size) {
    onProgress?.(size, size)
    return
  }

  const writable = await fileHandle.createWritable({ keepExistingData: true })
  try {
    while (offset < size) {
      const batch = []
      let batchOffset = offset
      for (let i = 0; i < PARALLEL_CHUNKS && batchOffset < size; i += 1) {
        const start = batchOffset
        const end = Math.min(start + CHUNK_SIZE, size) - 1
        batch.push({ start, end })
        batchOffset = end + 1
      }

      const buffers = await Promise.all(batch.map(async ({ start, end }) => {
        const response = await fetch(fileUrl, {
          headers: { Range: `bytes=${start}-${end}` },
          cache: 'no-store',
        })
        if (!response.ok && response.status !== 206) {
          throw new Error(`${t('downloadHttpFailed')}${response.status}`)
        }
        if (response.status === 200 && start > 0) {
          throw new Error(t('resumeUnsupported'))
        }
        const buffer = await response.arrayBuffer()
        if (!buffer.byteLength) throw new Error(t('emptyResponse'))
        return { start, buffer }
      }))

      for (const { start, buffer } of buffers) {
        await writable.seek(start)
        await writable.write(buffer)
        offset = start + buffer.byteLength
        onProgress?.(Math.max(offset, batchOffset), size)
      }
      offset = batchOffset
    }
    await writable.close()
  } catch (err) {
    try { await writable.abort() } catch {}
    throw err
  }
}

