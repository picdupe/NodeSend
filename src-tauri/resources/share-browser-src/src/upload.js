import { t } from './i18n'
import { withAccessToken } from './auth'

const CHUNK_SIZE = 8 * 1024 * 1024

function sessionKey(directory, file) {
  return `netshare-upload:${directory}:${file.name}:${file.size}:${file.lastModified}`
}

function joinPath(...parts) {
  return parts.filter(Boolean).join('/').replace(/\\+/g, '/')
}

async function initUpload(directory, file) {
  const key = sessionKey(directory, file)
  const uploadId = localStorage.getItem(key)
  const response = await fetch(withAccessToken('/api/upload/init'), {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ upload_id: uploadId || null, directory, name: file.name, size: file.size }),
  })
  if (!response.ok) throw new Error(await response.text())
  const data = await response.json()
  localStorage.setItem(key, data.upload_id)
  return { ...data, key }
}

export async function uploadFile(file, directory, onProgress) {
  const state = await initUpload(directory, file)
  let offset = Math.min(state.offset || 0, file.size)
  if (offset === file.size) {
    localStorage.removeItem(state.key)
    onProgress?.(file.size)
    return
  }

  while (offset < file.size) {
    const end = Math.min(offset + CHUNK_SIZE, file.size)
    const response = await fetch(withAccessToken(`/api/upload/${encodeURIComponent(state.upload_id)}`), {
      method: 'PUT',
      headers: {
        'Content-Range': `bytes ${offset}-${end - 1}/${file.size}`,
        'Content-Type': 'application/octet-stream',
      },
      body: file.slice(offset, end),
    })
    if (!response.ok) throw new Error(await response.text())
    const result = await response.json()
    if (result.offset <= offset) throw new Error(t('uploadNoProgress'))
    offset = result.offset
    onProgress?.(offset)
  }
  localStorage.removeItem(state.key)
}

export function relativeDirectory(file, rootDirectory) {
  const relative = file.webkitRelativePath || file.name
  const parts = relative.split('/').filter(Boolean)
  parts.pop()
  return joinPath(rootDirectory, ...parts)
}

