import { t } from './i18n'
import { downloadFileToHandle } from './fileDownload'
import { withAccessToken } from './auth'

async function fetchDirectoryTree(path, encodePath, apiUrl) {
  // The tree endpoint intentionally returns one directory level at a time.
  // Fetch each descendant as it is visited so folder downloads still include
  // every nested file without forcing the normal directory view to load a
  // whole subtree.
  const response = await fetch(apiUrl(`/api/list?path=${encodePath(path)}`), { cache: 'no-store' })
  if (!response.ok) throw new Error(await response.text())
  return response.json()
}

async function collectDownloadFiles(path, encodePath, apiUrl) {
  const tree = await fetchDirectoryTree(path, encodePath, apiUrl)
  const files = []
  const directories = []

  async function walk(node, loaded = false) {
    if (node.type === 'file') {
      files.push(node)
      return
    }

    const directory = loaded ? node : await fetchDirectoryTree(node.path, encodePath, apiUrl)
    for (const child of directory.children || []) {
      if (child.type === 'file') files.push(child)
      else if (child.type === 'dir') {
        directories.push(child)
        await walk(child)
      }
    }
  }

  directories.push({ path: tree.path, name: tree.name, type: 'dir' })
  await walk(tree, true)
  return { tree, files, directories }
}

export async function downloadFolder(item, { encodePath, fileUrl, apiUrl = withAccessToken, onProgress }) {
  if (!item || item.type !== 'dir') return
  if (!window.isSecureContext) {
    throw new Error(t('localHttpFolderPicker'))
  }
  if (typeof window.showDirectoryPicker !== 'function') {
    throw new Error(t('folderPickerUnsupported'))
  }

  const targetRoot = await window.showDirectoryPicker({ mode: 'readwrite' })
  const { tree, files, directories } = await collectDownloadFiles(item.path, encodePath, apiUrl)
  const folderHandle = await targetRoot.getDirectoryHandle(tree.name, { create: true })
  const handles = new Map([[tree.path, folderHandle]])

  for (const directory of directories) {
    if (directory.path === tree.path) continue
    const parentPath = directory.path.split('/').slice(0, -1).join('/')
    const parentHandle = handles.get(parentPath)
    if (!parentHandle) throw new Error(`${t('createFolderFailed')}${directory.path}`)
    handles.set(
      directory.path,
      await parentHandle.getDirectoryHandle(directory.name, { create: true }),
    )
  }

  onProgress?.({ active: true, current: '', completed: 0, total: files.length })

  for (const file of files) {
    const parentPath = file.path.split('/').slice(0, -1).join('/')
    const parentHandle = handles.get(parentPath)
    if (!parentHandle) throw new Error(`${t('locateFolderFailed')}${file.path}`)

    onProgress?.({ current: file.path })
    const fileHandle = await parentHandle.getFileHandle(file.name, { create: true })
    await downloadFileToHandle(
      fileHandle,
      fileUrl(file),
      (completed, total) => onProgress?.({ current: file.path, fileCompleted: completed, fileTotal: total }),
    )
    onProgress?.({ completed: 1 })
  }

  onProgress?.({ active: false, current: '' })
}

