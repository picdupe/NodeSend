import { t } from './i18n'
import { downloadFileToHandle } from './fileDownload'
import { relativeDirectory, uploadFile } from './upload'

export async function handleUploadChange(event, currentPath, uploadProgress, error, loadTree) {
  const files = [...(event.target.files || [])]
  event.target.value = ''
  if (!files.length) return

  uploadProgress.value = {
    active: true,
    current: '',
    completed: 0,
    total: files.length,
    bytes: 0,
    size: files.reduce((sum, file) => sum + file.size, 0),
    status: 'active',
  }

  try {
    for (const file of files) {
      const directory = relativeDirectory(file, currentPath.value)
      uploadProgress.value.current = file.webkitRelativePath || file.name
      uploadProgress.value.bytes = 0
      uploadProgress.value.size = file.size
      await uploadFile(file, directory, bytes => {
        uploadProgress.value.bytes = bytes
      })
      uploadProgress.value.completed += 1
    }
    await loadTree(currentPath.value)
  } catch (err) {
    error.value = `${t('uploadFailed')}${err.message || err}`
  } finally {
    uploadProgress.value.active = false
    if (uploadProgress.value.completed === uploadProgress.value.total) {
      uploadProgress.value.status = 'completed'
    }
  }
}

export async function downloadFileWithResume(item, fileUrl, uploadProgress, error) {
  if (!window.isSecureContext || typeof window.showSaveFilePicker !== 'function') {
    window.location.href = fileUrl(item)
    return
  }

  try {
    const handle = await window.showSaveFilePicker({ suggestedName: item.name })
    await downloadFileToHandle(handle, fileUrl(item), (bytes, total) => {
      uploadProgress.value = {
        active: true,
        current: item.name,
        completed: 0,
        total: 1,
        bytes,
        size: total,
        status: 'active',
      }
    })
    uploadProgress.value.status = 'completed'
    uploadProgress.value.completed = 1
  } catch (err) {
    if (err?.name !== 'AbortError') {
      error.value = `${t('fileDownloadFailed')}${err.message || err}`
    }
  } finally {
    uploadProgress.value.active = false
  }
}

