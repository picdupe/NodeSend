<script setup>
import { computed, nextTick, onMounted, onUnmounted, ref } from 'vue'
import iconUrl from '../assets/nodesend-mark.svg'
import { downloadFolder as downloadFolderToDirectory } from './folderDownload'
import { handleUploadChange as processUploadChange, downloadFileWithResume as processDownloadFile } from './transferActions'
import { activeLanguage, setLanguage as updateLanguage, t as translate } from './i18n'
import { withAccessToken } from './auth'

const root = ref(null)
const currentPath = ref('')
const query = ref('')
const items = ref([])
const currentDirectoryItems = ref([])
const viewMode = ref(localStorage.getItem('netshare-view-mode') || 'card')
const sortMode = ref(localStorage.getItem('netshare-sort-mode') || 'name-asc')
const loading = ref(false)
const error = ref('')
const languageRevision = ref(0)
const softwareLanguage = ref(activeLanguage())
const storedBrowserLanguage = localStorage.getItem('nodesend-browser-language')
const languagePreference = ref(
  ['zh-CN', 'zh-TW', 'en-US', 'ja-JP'].includes(storedBrowserLanguage) ? storedBrowserLanguage : 'software',
)
const viewerOpen = ref(false)
const viewerIndex = ref(-1)
const viewerImages = ref([])
const viewerZoom = ref(1)
const viewerUiVisible = ref(true)
const viewerFullscreen = ref(false)
const viewerElement = ref(null)
const viewerTopActions = ref(null)
const viewerStage = ref(null)
const viewerPrev = ref(null)
const viewerNext = ref(null)
const viewerCountElement = ref(null)
const viewerImageBounds = ref({ width: 0, height: 0 })
let viewerResizeObserver = null
let eventSocket = null
let eventReconnectTimer = null
let eventRefreshTimer = null
let eventRefreshPending = false
let disposed = false
let listRequestId = 0

function t(key) {
  languageRevision.value
  return translate(key)
}

function applyBrowserLanguage(language) {
  updateLanguage(language)
  document.documentElement.lang = activeLanguage()
  window.dispatchEvent(new CustomEvent('nodesend-language-changed', { detail: activeLanguage() }))
}

function applySoftwareLanguage(language) {
  softwareLanguage.value = language
  if (languagePreference.value === 'software') applyBrowserLanguage(language)
}

function changeBrowserLanguage(event) {
  const selected = event.target.value
  languagePreference.value = selected
  if (selected === 'software') {
    localStorage.removeItem('nodesend-browser-language')
    applyBrowserLanguage(softwareLanguage.value)
    return
  }
  localStorage.setItem('nodesend-browser-language', selected)
  applyBrowserLanguage(selected)
}

if (languagePreference.value !== 'software') {
  applyBrowserLanguage(languagePreference.value)
}

function handleLanguageChanged() {
  languageRevision.value += 1
}

window.addEventListener('nodesend-language-changed', handleLanguageChanged)
const contextMenu = ref({ visible: false, x: 0, y: 0, item: null })
const shareStatus = ref(null)
const sharedFolder = ref('')
const sharedFolderName = computed(() => {
  const path = sharedFolder.value.replace(/[\\/]+$/, '')
  return path.split(/[\\/]/).filter(Boolean).pop() || t('folder')
})
const folderDownload = ref({ active: false, current: '', completed: 0, total: 0 })
const folderDownloadDialog = ref({ visible: false, item: null })
const uploadInput = ref(null)
const uploadFolderInput = ref(null)
const uploadProgress = ref({
  active: false,
  current: '',
  completed: 0,
  total: 0,
  bytes: 0,
  size: 0,
  status: 'idle',
})
const transferPanelOpen = ref(false)

const imageExtensions = /\.(avif|bmp|gif|jpe?g|png|webp|3fr|arw|cr2|cr3|dcr|dng|erf|kdc|mrw|nef|nrw|orf|pef|raf|raw|rw2|rwl|srw|x3f)$/i

const displayedItems = computed(() => [...items.value].sort((a, b) => {
  if (a.type !== b.type) return a.type === 'dir' ? -1 : 1
  if (sortMode.value.startsWith('time')) {
    const difference = (Number(a.modified) || 0) - (Number(b.modified) || 0)
    if (difference) return sortMode.value === 'time-desc' ? -difference : difference
  }
  const difference = String(a.name || '').localeCompare(String(b.name || ''), undefined, { numeric: true, sensitivity: 'base' })
  return sortMode.value === 'name-desc' ? -difference : difference
}))

const imageItems = computed(() => displayedItems.value
  .filter(item => item.type === 'file' && imageExtensions.test(item.name))
  .map(item => ({
    ...item,
    url: withAccessToken(item.preview_url || `/preview?path=${encodePath(item.path)}`),
  })))

const currentImage = computed(() => viewerImages.value[viewerIndex.value] || null)
const viewerCount = computed(() => viewerImages.value.length ? `${viewerIndex.value + 1} / ${viewerImages.value.length}` : '')
const viewerImageStyle = computed(() => {
  if (!viewerImageBounds.value.width || !viewerImageBounds.value.height) return {}
  return {
    maxWidth: `${viewerImageBounds.value.width}px`,
    maxHeight: `${viewerImageBounds.value.height}px`,
  }
})

function formatSize(bytes) {
  if (bytes < 1024) return `${bytes}B`
  const units = ['KB', 'MB', 'GB', 'TB']
  let value = bytes
  let unit = -1
  do {
    value /= 1024
    unit += 1
  } while (value >= 1024 && unit < units.length - 1)
  return `${value.toFixed(2).replace(/\.00$/, '').replace(/(\.\d)0$/, '$1')}${units[unit]}`
}

function isImage(name) {
  return imageExtensions.test(name)
}

function setSort(mode) {
  sortMode.value = mode
  localStorage.setItem('netshare-sort-mode', mode)
}

function encodePath(path) {
  return encodeURIComponent(path || '')
}

const breadcrumbItems = computed(() => {
  const parts = currentPath.value.split('/').filter(Boolean)
  let path = ''
  return parts.map(name => {
    path = path ? `${path}/${name}` : name
    return { name, path }
  })
})

function fileUrl(item) {
  return withAccessToken(item.url || `/download?path=${encodePath(item.path)}`)
}

function downloadHttp(item) {
  if (!item) return
  const anchor = document.createElement('a')
  anchor.href = item.type === 'dir' ? folderArchiveUrl(item) : fileUrl(item)
  anchor.download = item.name || ''
  anchor.style.display = 'none'
  document.body.appendChild(anchor)
  anchor.click()
  anchor.remove()
}

function nodeSendUrl(item, directory, ack) {
  const status = shareStatus.value
  if (!status?.node_id || !status?.tcp_port || !status?.share_id || !item?.path) return null
  const params = new URLSearchParams({
    node: status.node_id,
    host: window.location.hostname,
    tcp: String(status.tcp_port),
    quic: String(status.quic_port || 0),
    share: status.share_id,
    path: item.path,
    directory: directory ? '1' : '0',
  })
  const accessToken = new URLSearchParams(window.location.search).get('token')
  if (accessToken) params.set('token', accessToken)
  if (ack) {
    params.set('ack', ack)
    params.set('ackUrl', new URL(withAccessToken(`/api/web-downloads/ack/${ack}`), window.location.href).href)
  }
  if (!directory && Number.isFinite(Number(item.size))) params.set('size', String(item.size))
  return `ns://download?${params}`
}

function randomAckToken() {
  return Array.from(crypto.getRandomValues(new Uint8Array(16)), value => value.toString(16).padStart(2, '0')).join('')
}

function launchNodeSend(item, directory) {
  error.value = ''
  const ack = randomAckToken()
  const uri = nodeSendUrl(item, directory, ack)
  if (!uri) {
    error.value = t('nodeSendDownloadUnconfirmed')
    return
  }
  const frame = document.createElement('iframe')
  frame.style.display = 'none'
  frame.src = uri
  document.body.appendChild(frame)
  window.setTimeout(() => frame.remove(), 1500)
  const deadline = Date.now() + 10000
  const poll = async () => {
    if (Date.now() >= deadline) {
      error.value = t('nodeSendDownloadUnconfirmed')
      return
    }
    try {
      const response = await fetch(withAccessToken(`/api/web-downloads/ack/${ack}`), { cache: 'no-store' })
      if (response.status === 204) return
    } catch {}
    window.setTimeout(poll, 200)
  }
  void poll()
  return true
}

function folderArchiveUrl(item) {
  return withAccessToken(`/download-folder?path=${encodePath(item.path)}`)
}

function openFolderDownloadDialog(item) {
  if (!item || item.type !== 'dir') return
  folderDownloadDialog.value = { visible: true, item }
}

function closeFolderDownloadDialog() {
  folderDownloadDialog.value.visible = false
  folderDownloadDialog.value.item = null
}

function startFolderArchiveDownload() {
  const item = folderDownloadDialog.value.item
  if (!item) return
  closeFolderDownloadDialog()
  window.location.href = folderArchiveUrl(item)
}

async function downloadFolder(item) {
  if (!item || item.type !== 'dir') return
  try {
    error.value = ''
    folderDownload.value = { active: false, current: '', completed: 0, total: 0 }
    await downloadFolderToDirectory(item, {
      encodePath,
      fileUrl,
      apiUrl: withAccessToken,
      onProgress(progress) {
        folderDownload.value.active = progress.active ?? folderDownload.value.active
        if (progress.current !== undefined) folderDownload.value.current = progress.current
        if (progress.total !== undefined) folderDownload.value.total = progress.total
        if (progress.completed) folderDownload.value.completed += progress.completed
      },
    })
  } catch (err) {
    if (err?.name !== 'AbortError') error.value = `${t('folderDownloadFailed')}${err.message || err}`
  } finally {
    folderDownload.value.active = false
    folderDownload.value.current = ''
  }
}

function downloadItem(item) {
  if (!item) return
  if (item.type === 'dir') {
    openFolderDownloadDialog(item)
    return
  }
  downloadFileWithResume(item)
}

function chooseUploadFiles() {
  uploadInput.value?.click()
}

function chooseUploadFolder() {
  uploadFolderInput.value?.click()
}

function toggleTransferPanel() {
  transferPanelOpen.value = !transferPanelOpen.value
}

async function handleUploadChange(event) {
  await processUploadChange(event, currentPath, uploadProgress, error, loadTree)
}

async function downloadFileWithResume(item) {
  await processDownloadFile(item, fileUrl, uploadProgress, error)
}

function openDirectory(path) {
  const nextPath = path || ''
  query.value = ''
  if (nextPath === currentPath.value) {
    void loadTree(nextPath, true)
    return
  }
  const url = new URL(window.location.href)
  if (nextPath) url.searchParams.set('path', nextPath)
  else url.searchParams.delete('path')
  url.searchParams.delete('rfsh')
  window.history.pushState({}, '', url)
  loadTree(nextPath)
}

const shareRootPath = window.location.pathname.replace(/\/+$/, '') || '/'

function shareDirectoryUrl(path) {
  return withAccessToken(path ? `${shareRootPath}?path=${encodePath(path)}` : shareRootPath)
}

function goParent() {
  if (!currentPath.value) return
  const parts = currentPath.value.split('/').filter(Boolean)
  parts.pop()
  openDirectory(parts.join('/'))
}

function updateUrlPath(path) {
  const url = new URL(window.location.href)
  if (path) url.searchParams.set('path', path)
  else url.searchParams.delete('path')
  url.searchParams.delete('rfsh')
  window.history.replaceState({}, '', url)
}

function applyStatus(status) {
  if (!status) return
  if (status.language) {
    softwareLanguage.value = status.language
    if (languagePreference.value === 'software') applyBrowserLanguage(status.language)
  }
  const changed = sharedFolder.value && sharedFolder.value !== status.shared_folder
  shareStatus.value = status
  sharedFolder.value = status.shared_folder
  if (changed) {
    currentDirectoryItems.value = []
    currentPath.value = ''
    updateUrlPath('')
    void loadTree('')
  }
}

async function loadTree(path = currentPath.value, refresh = false, silent = false) {
  const requestId = ++listRequestId
  if (!silent) {
    loading.value = true
    error.value = ''
  }
  currentPath.value = path || ''
  try {
    const response = await fetch(withAccessToken(`/api/list?path=${encodePath(currentPath.value)}${refresh ? '&rfsh' : ''}`), { cache: 'no-store' })
    if (!response.ok) throw new Error(await response.text())
    const tree = await response.json()
    if (disposed || requestId !== listRequestId) return
    currentPath.value = tree.path || ''
    currentDirectoryItems.value = tree.children || []
    items.value = currentDirectoryItems.value
    updateUrlPath(currentPath.value)
  } catch (err) {
    if (disposed || requestId !== listRequestId) return
    if (!silent) {
      currentDirectoryItems.value = []
      items.value = []
      error.value = `${t('loadFailed')}${err.message}`
    }
  } finally {
    if (!disposed && requestId === listRequestId) {
      if (!silent) loading.value = false
      flushTreeRefresh()
    }
  }
}

function refreshDirectory() {
  query.value = ''
  return loadTree(currentPath.value, true)
}

async function search() {
  const q = query.value.trim().toLowerCase()
  if (!q) {
    ++listRequestId
    loading.value = false
    error.value = ''
    items.value = currentDirectoryItems.value
    flushTreeRefresh()
    return
  }
  const requestId = ++listRequestId
  loading.value = false
  error.value = ''
  const searchText = query.value.trim().toLowerCase()
  if (!searchText) {
    items.value = currentDirectoryItems.value
    return
  }
  const params = new URLSearchParams({ q: searchText, path: currentPath.value })
  try {
    const response = await fetch(withAccessToken(`/api/search?${params}`), { cache: 'no-store' })
    if (!response.ok) throw new Error(await response.text())
    const results = await response.json()
    if (disposed || requestId !== listRequestId) return
    items.value = Array.isArray(results) ? results : []
  } catch (err) {
    if (disposed || requestId !== listRequestId) return
    items.value = []
    error.value = `${t('searchFailed')}${err.message || err}`
  }
}

function clearSearch() {
  query.value = ''
  ++listRequestId
  loading.value = false
  error.value = ''
  items.value = currentDirectoryItems.value
  flushTreeRefresh()
}

function connectEventSocket() {
  if (disposed) return
  if (eventSocket && eventSocket.readyState <= WebSocket.OPEN) return
  const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:'
  eventSocket = new WebSocket(`${protocol}//${window.location.host}${withAccessToken('/api/events')}`)
  eventSocket.onopen = () => {}
  eventSocket.onmessage = event => {
    try {
      const message = JSON.parse(event.data)
      if (message.type === 'status') {
        applyStatus(message.data)
      } else if (message.type === 'language') {
        applySoftwareLanguage(message.data)
      } else if (message.type === 'tree_changed') {
        if (affectsCurrentDirectory(message.paths)) scheduleTreeRefresh()
      }
    } catch {}
  }
  eventSocket.onclose = () => {
    eventSocket = null
    if (!disposed) eventReconnectTimer = window.setTimeout(connectEventSocket, 1000)
  }
  eventSocket.onerror = () => eventSocket?.close()
}

function affectsCurrentDirectory(paths) {
  if (!Array.isArray(paths) || !paths.length) return true
  const current = currentPath.value.replace(/^\/+|\/+$/g, '')
  return paths.some(value => {
    const changed = String(value || '').replace(/^\/+|\/+$/g, '')
    if (!changed || changed === '.') return true
    const parent = changed.includes('/') ? changed.slice(0, changed.lastIndexOf('/')) : ''
    return changed === current || parent === current || current.startsWith(changed + '/')
  })
}

function scheduleTreeRefresh() {
  if (disposed) return
  eventRefreshPending = true
  if (eventRefreshTimer) window.clearTimeout(eventRefreshTimer)
  eventRefreshTimer = window.setTimeout(() => {
    eventRefreshTimer = null
    if (disposed || query.value || viewerOpen.value || loading.value) return
    eventRefreshPending = false
    loadTree(currentPath.value, false, true)
  }, 600)
}

function flushTreeRefresh() {
  if (eventRefreshPending && !query.value && !viewerOpen.value && !loading.value) {
    scheduleTreeRefresh()
  }
}

function openViewer(item) {
  const index = imageItems.value.findIndex(image => image.path === item.path)
  if (index < 0) return
  viewerImages.value = imageItems.value
  viewerIndex.value = index
  viewerZoom.value = 1
  viewerUiVisible.value = true
  viewerFullscreen.value = false
  viewerOpen.value = true
  document.body.classList.add('viewer-lock')
  nextTick(startViewerResizeObserver)
}

function closeViewer() {
  if (document.fullscreenElement) document.exitFullscreen().catch(() => {})
  viewerOpen.value = false
  viewerIndex.value = -1
  viewerImages.value = []
  viewerUiVisible.value = true
  viewerFullscreen.value = false
  viewerImageBounds.value = { width: 0, height: 0 }
  viewerResizeObserver?.disconnect()
  viewerResizeObserver = null
  document.body.classList.remove('viewer-lock')
  flushTreeRefresh()
}

function previousImage() {
  if (!viewerImages.value.length) return
  viewerIndex.value = (viewerIndex.value - 1 + viewerImages.value.length) % viewerImages.value.length
  viewerZoom.value = 1
  if (viewerFullscreen.value) viewerUiVisible.value = false
}

function nextImage() {
  if (!viewerImages.value.length) return
  viewerIndex.value = (viewerIndex.value + 1) % viewerImages.value.length
  viewerZoom.value = 1
  if (viewerFullscreen.value) viewerUiVisible.value = false
}

function handleViewerWheel(event) {
  if (!viewerOpen.value) return
  event.preventDefault()
  if (event.ctrlKey) {
    const factor = event.deltaY < 0 ? 1.15 : 1 / 1.15
    viewerZoom.value = Math.min(5, Math.max(0.5, viewerZoom.value * factor))
  } else if (event.deltaY > 0) {
    nextImage()
  } else if (event.deltaY < 0) {
    previousImage()
  }
}

async function toggleFullscreen() {
  try {
    if (!document.fullscreenElement) await document.querySelector('.viewer')?.requestFullscreen()
    else await document.exitFullscreen()
  } catch {}
}

function handleFullscreenChange() {
  viewerFullscreen.value = Boolean(document.fullscreenElement)
  viewerUiVisible.value = true
  nextTick(updateViewerImageBounds)
}

function updateViewerImageBounds() {
  if (!viewerOpen.value || viewerFullscreen.value) return

  const viewer = viewerElement.value
  const stage = viewerStage.value
  const topActions = viewerTopActions.value
  const prev = viewerPrev.value
  const next = viewerNext.value
  const count = viewerCountElement.value
  if (!viewer || !stage || !topActions || !prev || !next || !count) return

  const viewerRect = viewer.getBoundingClientRect()
  const stageRect = stage.getBoundingClientRect()
  const topRect = topActions.getBoundingClientRect()
  const prevRect = prev.getBoundingClientRect()
  const nextRect = next.getBoundingClientRect()
  const countRect = count.getBoundingClientRect()

  const topControls = [...topActions.querySelectorAll('.viewer-control')]
  const topControlBottom = topControls.reduce(
    (bottom, control) => Math.max(bottom, control.getBoundingClientRect().bottom),
    topRect.bottom,
  )

  const horizontalGap = 8
  const verticalGap = 4
  const left = Math.max(stageRect.left, prevRect.right + horizontalGap)
  const right = Math.min(stageRect.right, nextRect.left - horizontalGap)
  const top = Math.max(stageRect.top, topControlBottom + verticalGap)
  const bottom = Math.min(stageRect.bottom, countRect.top - verticalGap)

  const width = Math.max(0, Math.min(stageRect.width, right - left))
  const height = Math.max(0, Math.min(stageRect.height, bottom - top))

  viewerImageBounds.value = {
    width: Math.max(0, Math.min(width, viewerRect.width)),
    height: Math.max(0, Math.min(height, viewerRect.height)),
  }
}

function startViewerResizeObserver() {
  if (viewerResizeObserver) viewerResizeObserver.disconnect()
  if (typeof ResizeObserver === 'undefined') return
  viewerResizeObserver = new ResizeObserver(() => updateViewerImageBounds())
  if (viewerElement.value) viewerResizeObserver.observe(viewerElement.value)
  if (viewerTopActions.value) viewerResizeObserver.observe(viewerTopActions.value)
  if (viewerStage.value) viewerResizeObserver.observe(viewerStage.value)
  if (viewerCountElement.value) viewerResizeObserver.observe(viewerCountElement.value)
  nextTick(updateViewerImageBounds)
}

function handleViewerClick(event) {
  if (viewerFullscreen.value) {
    toggleFullscreenViewerUi()
    return
  }
  if (event.target.closest?.('.viewer-image, .viewer-control')) return
  closeViewer()
}

function handleKeydown(event) {
  if (!viewerOpen.value) return
  if (event.key === 'Escape') closeViewer()
  else if (event.key === 'ArrowLeft') previousImage()
  else if (event.key === 'ArrowRight') nextImage()
}

function setView(mode) {
  viewMode.value = mode
  localStorage.setItem('netshare-view-mode', mode)
}

function downloadCurrentImage() {
  if (!currentImage.value) return
  window.location.href = withAccessToken(`/download?path=${encodePath(currentImage.value.path)}`)
}

function showCardContextMenu(event, item) {
  const menuWidth = 180
  const menuHeight = item && isImage(item.name) ? 192 : 90
  contextMenu.value = {
    visible: true,
    x: Math.max(8, Math.min(event.clientX, window.innerWidth - menuWidth - 8)),
    y: Math.max(8, Math.min(event.clientY, window.innerHeight - menuHeight - 8)),
    item,
  }
}

function closeContextMenu() {
  contextMenu.value.visible = false
  contextMenu.value.item = null
}

function downloadCardFile() {
  const item = contextMenu.value.item
  if (!item) return
  downloadItem(item)
  closeContextMenu()
}

function downloadCardWithNodeSend() {
  const item = contextMenu.value.item
  if (!item) return
  closeContextMenu()
  const directory = item.type === 'dir'
  launchNodeSend(item, directory)
}

async function saveContextImage() {
  const item = contextMenu.value.item
  if (!item) return
  closeContextMenu()
  await processDownloadFile(item, fileUrl, uploadProgress, error)
}

async function copyContextImage() {
  const item = contextMenu.value.item
  if (!item) return
  const current = viewerOpen.value && currentImage.value?.path === item.path
    ? document.querySelector('.viewer-image')
    : null
  closeContextMenu()
  try {
    if (!navigator.clipboard?.write || typeof ClipboardItem === 'undefined') {
      throw new Error(t('imageCopyUnsupported'))
    }
    const toPng = bitmap => new Promise((resolve, reject) => {
      const canvasFallback = () => {
        const canvas = document.createElement('canvas')
        canvas.width = bitmap.width
        canvas.height = bitmap.height
        const context = canvas.getContext('2d')
        if (!context) {
          bitmap.close()
          reject(new Error(t('imageConversionFailed')))
          return
        }
        context.drawImage(bitmap, 0, 0)
        bitmap.close()
        canvas.toBlob(blob => {
          canvas.width = 0
          canvas.height = 0
          blob ? resolve(blob) : reject(new Error(t('imageConversionFailed')))
        }, 'image/png')
      }
      if (typeof Worker === 'undefined' || typeof OffscreenCanvas === 'undefined') {
        canvasFallback()
        return
      }
      const workerSource = 'self.onmessage=async event=>{const bitmap=event.data;try{const canvas=new OffscreenCanvas(bitmap.width,bitmap.height);canvas.getContext("2d").drawImage(bitmap,0,0);self.postMessage({blob:await canvas.convertToBlob({type:"image/png"})})}catch(error){self.postMessage({error:String(error)})}finally{bitmap.close()}}'
      let worker
      let objectUrl
      try {
        objectUrl = URL.createObjectURL(new Blob([workerSource], { type: 'text/javascript' }))
        worker = new Worker(objectUrl)
        URL.revokeObjectURL(objectUrl)
      } catch {
        if (objectUrl) URL.revokeObjectURL(objectUrl)
        canvasFallback()
        return
      }
      worker.onmessage = event => {
        worker.terminate()
        event.data.error ? reject(new Error(event.data.error)) : resolve(event.data.blob)
      }
      worker.onerror = event => {
        worker.terminate()
        reject(new Error(event.message || t('imageConversionFailed')))
      }
      try {
        worker.postMessage(bitmap, [bitmap])
      } catch (err) {
        worker.terminate()
        bitmap.close()
        reject(err)
      }
    })
    const imagePromise = (async () => {
      let bitmap
      if (current?.complete && current.naturalWidth) {
        bitmap = await createImageBitmap(current)
      } else {
        const response = await fetch(fileUrl(item), { cache: 'force-cache' })
        if (!response.ok) throw new Error(`HTTP ${response.status}`)
        bitmap = await createImageBitmap(await response.blob())
      }
      return toPng(bitmap)
    })()
    await navigator.clipboard.write([new ClipboardItem({ 'image/png': imagePromise })])
  } catch (err) {
    error.value = err?.message || String(err)
  }
}

async function copyContextImageAddress() {
  const item = contextMenu.value.item
  if (!item) return
  closeContextMenu()
  try {
    const address = new URL(fileUrl(item), window.location.href).href
    if (navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(address)
    } else {
      const input = document.createElement('textarea')
      input.value = address
      input.style.position = 'fixed'
      input.style.opacity = '0'
      document.body.appendChild(input)
      input.select()
      if (!document.execCommand('copy')) throw new Error(t('copyFailed'))
      input.remove()
    }
  } catch (err) {
    error.value = err?.message || String(err)
  }
}

function handlePageContextMenu(event) {
  const card = event.target.closest?.('.entry-card')
  const viewerImage = event.target.closest?.('.viewer-image')
  if (!card && !viewerImage) return
  event.preventDefault()
  event.stopPropagation()
  const item = viewerImage
    ? currentImage.value
    : items.value.find(entry => entry.path === card.dataset.path)
  if (item) showCardContextMenu(event, item)
}

function toggleFullscreenViewerUi() {
  viewerUiVisible.value = !viewerUiVisible.value
}

function handlePopState() {
  query.value = ''
  const params = new URLSearchParams(window.location.search)
  loadTree(params.get('path') || '')
}

onMounted(() => {
  const params = new URLSearchParams(window.location.search)
  currentPath.value = params.get('path') || ''
  const refreshRequested = params.has('rfsh')
  connectEventSocket()
  void loadTree(currentPath.value, refreshRequested)
  window.addEventListener('keydown', handleKeydown)
  window.addEventListener('popstate', handlePopState)
  window.addEventListener('click', closeContextMenu)
  window.addEventListener('contextmenu', handlePageContextMenu, true)
  document.addEventListener('fullscreenchange', handleFullscreenChange)
  window.addEventListener('resize', updateViewerImageBounds)
})

onUnmounted(() => {
  window.removeEventListener('nodesend-language-changed', handleLanguageChanged)
  disposed = true
  listRequestId += 1
  window.removeEventListener('keydown', handleKeydown)
  window.removeEventListener('popstate', handlePopState)
  window.removeEventListener('click', closeContextMenu)
  window.removeEventListener('contextmenu', handlePageContextMenu, true)
  document.removeEventListener('fullscreenchange', handleFullscreenChange)
  window.removeEventListener('resize', updateViewerImageBounds)
  viewerResizeObserver?.disconnect()
  viewerResizeObserver = null
  if (eventReconnectTimer) window.clearTimeout(eventReconnectTimer)
  if (eventRefreshTimer) window.clearTimeout(eventRefreshTimer)
  eventSocket?.close()
  eventSocket = null
})
</script>

<template>
  <div class="app-shell">
    <header class="topbar">
      <div class="brand">
        <img class="brand-mark" :src="iconUrl" alt="NodeSend" />
        <div>
          <div class="brand-name">NodeSend</div>
          <div class="brand-subtitle">{{ t('fileSharing') }}</div>
        </div>
      </div>
      <div class="topbar-actions">
        <select class="language-select" :value="languagePreference" :aria-label="t('language')" @change="changeBrowserLanguage">
          <option value="software">{{ t('followSoftware') }}</option>
          <option value="zh-CN">简体中文</option>
          <option value="zh-TW">繁體中文</option>
          <option value="en-US">English</option>
          <option value="ja-JP">日本語</option>
        </select>
        <div class="status-chip">
          <span class="status-dot"></span>
          {{ t('sharing') }}
        </div>
      </div>
    </header>

    <main>
      <section class="hero-card">
        <div class="hero-title">{{ t('folder') }}</div>
        <div class="hero-path" :title="sharedFolder">{{ sharedFolder || t('loading') }}</div>
      </section>

      <section class="toolbar">
        <div class="search-box">
          <span class="search-icon">⌕</span>
          <input
            v-model="query"
            :placeholder="t('searchPlaceholder')"
            @keyup.enter="search"
          />
          <button v-if="query" class="clear-button" @click="clearSearch">×</button>
          <button class="search-button" @click="search">{{ t('search') }}</button>
        </div>
        <div class="view-switch">
          <button class="upload-button upload-file-button" @click="chooseUploadFiles">{{ t('uploadFile') }}</button>
          <button class="upload-button upload-folder-button" @click="chooseUploadFolder">{{ t('uploadFolder') }}</button>
          <button class="transfer-button" :class="{ active: transferPanelOpen, busy: uploadProgress.active }" @click="toggleTransferPanel">
            {{ t('transferProgress') }}<span v-if="uploadProgress.active" class="transfer-badge">{{ t('inProgress') }}</span>
          </button>
          <select class="sort-select" :value="sortMode" :aria-label="t('sort')" @change="setSort($event.target.value)">
            <option value="name-asc">{{ t('nameAsc') }}</option>
            <option value="name-desc">{{ t('nameDesc') }}</option>
            <option value="time-desc">{{ t('timeDesc') }}</option>
            <option value="time-asc">{{ t('timeAsc') }}</option>
          </select>
          <button :class="{ active: viewMode === 'list' }" @click="setView('list')">{{ t('list') }}</button>
          <button :class="{ active: viewMode === 'card' }" @click="setView('card')">{{ t('card') }}</button>
        </div>
        <input ref="uploadInput" class="hidden-input" type="file" multiple @change="handleUploadChange" />
        <input ref="uploadFolderInput" class="hidden-input" type="file" webkitdirectory directory multiple @change="handleUploadChange" />
      </section>

      <section class="breadcrumb-card">
        <button class="parent-button" :disabled="!currentPath" @click="goParent">←</button>
        <div class="breadcrumb-text">
          <a class="breadcrumb-root" :href="shareRootPath" @click.prevent="openDirectory('')">{{ sharedFolderName }}</a>
          <template v-for="item in breadcrumbItems" :key="item.path">
            <span class="breadcrumb-separator">/</span>
            <a class="breadcrumb-current" :href="shareDirectoryUrl(item.path)" @click.prevent="openDirectory(item.path)">{{ item.name }}</a>
          </template>
        </div>
        <button class="refresh-button" :title="t('refresh')" :disabled="loading" @click="refreshDirectory">↻</button>
      </section>

      <div v-if="error" class="error-card">{{ error }}</div>
      <div v-else-if="loading" class="empty-card">{{ t('loading') }}</div>
      <div v-else-if="!items.length" class="empty-card">{{ query ? t('noMatches') : t('empty') }}</div>

      <section v-else class="content-grid" :class="`view-${viewMode}`">
        <template v-for="item in displayedItems" :key="item.path">
          <article
            v-if="item.type === 'dir'"
            class="entry-card directory-entry"
            :data-path="item.path"
          >
            <div class="entry-icon folder-icon">▰</div>
            <div class="entry-main">
              <a class="entry-name" :href="item.url || shareDirectoryUrl(item.path)" @click.prevent="openDirectory(item.path)">{{ item.name }}</a>
              <div v-if="viewMode === 'list'" class="entry-meta">{{ t('folderItem') }}</div>
            </div>
            <button class="entry-action" @click.prevent="downloadItem(item)">{{ t('download') }}</button>
          </article>

          <article
            v-else
            class="entry-card file-entry"
            :class="{ 'image-entry': isImage(item.name) }"
            :data-path="item.path"
          >
            <button v-if="isImage(item.name)" class="preview-box image-preview" @click="openViewer(item)">
              <img
                :src="withAccessToken(item.preview_url || (/\.(3fr|arw|cr2|cr3|dcr|dng|erf|kdc|mrw|nef|nrw|orf|pef|raf|raw|rw2|rwl|srw|x3f)$/i.test(item.name) ? `/preview?path=${encodePath(item.path)}&thumbnail=true` : `/preview?path=${encodePath(item.path)}`))"
                :alt="item.name"
                loading="lazy"
              />
            </button>
            <div v-else class="preview-box file-preview">▤</div>
            <div class="entry-main">
              <div class="entry-name" :title="item.name">{{ item.name }}</div>
              <div class="entry-meta">
                <span v-if="item.size !== undefined">{{ formatSize(item.size) }}</span>
                <span v-if="viewMode === 'list' && item.modified"> · {{ new Date(item.modified * 1000).toLocaleString() }}</span>
              </div>
            </div>
            <button class="entry-action" @click="downloadItem(item)">{{ t('download') }}</button>
          </article>
        </template>
      </section>
    </main>

    <div
      v-if="viewerOpen"
      class="viewer"
      ref="viewerElement"
      :class="{ 'viewer-fullscreen': viewerFullscreen, 'viewer-ui-visible': viewerUiVisible }"
      @wheel="handleViewerWheel"
      @click="handleViewerClick"
    >
      <div class="viewer-stage" ref="viewerStage">
        <button ref="viewerPrev" class="viewer-control viewer-prev" @click.stop="previousImage">‹</button>
      <img
        v-if="currentImage"
        class="viewer-image"
        :style="{ ...viewerImageStyle, transform: `scale(${viewerZoom})` }"
        :src="currentImage.url"
        :alt="currentImage.name"
        loading="eager"
        fetchpriority="high"
        decoding="async"
      />
        <button ref="viewerNext" class="viewer-control viewer-next" @click.stop="nextImage">›</button>
      </div>
      <div class="viewer-top-actions" ref="viewerTopActions">
        <button class="viewer-control" :title="t('fullscreen')" @click.stop="toggleFullscreen">⛶</button>
        <button class="viewer-control" :title="t('download')" @click.stop="downloadFileWithResume(currentImage)">↓</button>
        <button class="viewer-control" :title="t('close')" @click.stop="closeViewer">×</button>
      </div>
      <div class="viewer-count" ref="viewerCountElement">{{ viewerCount }}</div>
    </div>

    <div
      v-if="contextMenu.visible"
      class="context-menu"
      :style="{ left: `${contextMenu.x}px`, top: `${contextMenu.y}px` }"
      @click.stop
    >
      <button class="context-menu-item" @click="downloadCardFile">{{ t('download') }}</button>
      <button class="context-menu-item" @click="downloadCardWithNodeSend">{{ t('useNodeSend') }}</button>
      <template v-if="contextMenu.item && isImage(contextMenu.item.name)">
        <button class="context-menu-item" @click="saveContextImage">{{ t('saveImageAs') }}</button>
        <button class="context-menu-item" @click="copyContextImage">{{ t('copyImage') }}</button>
        <button class="context-menu-item" @click="copyContextImageAddress">{{ t('copyImageAddress') }}</button>
      </template>
    </div>

    <div v-if="folderDownloadDialog.visible" class="folder-download-mask" @click.self="closeFolderDownloadDialog">
      <div class="folder-download-dialog" role="dialog" aria-modal="true">
        <div class="folder-download-title">{{ t('folderDownload') }}</div>
        <div class="folder-download-name" :title="folderDownloadDialog.item?.name">
          {{ folderDownloadDialog.item?.name }}
        </div>
        <div class="folder-download-options">
          <button class="folder-download-option" @click="downloadFolder(folderDownloadDialog.item); closeFolderDownloadDialog()">
            <strong>{{ t('folderDownload') }}</strong>
            <span>{{ t('chooseFolder') }}</span>
          </button>
          <button class="folder-download-option" @click="startFolderArchiveDownload">
            <strong>{{ t('archiveDownload') }}</strong>
            <span>{{ t('archiveDescription') }}</span>
          </button>
        </div>
        <button class="folder-download-cancel" @click="closeFolderDownloadDialog">{{ t('cancel') }}</button>
      </div>
    </div>

    <div v-if="folderDownload.active" class="download-progress">
      <div class="download-progress-title">{{ t('downloadingFolder') }}</div>
      <div class="download-progress-text">
        {{ folderDownload.completed }} / {{ folderDownload.total }}
        <span v-if="folderDownload.current"> · {{ folderDownload.current }}</span>
      </div>
    </div>

    <div v-if="transferPanelOpen" class="transfer-panel">
      <div class="transfer-panel-header">
        <div>
          <div class="download-progress-title">{{ t('transferProgress') }}</div>
          <div class="download-progress-text">
            {{ uploadProgress.status === 'completed' ? t('recentComplete') : uploadProgress.active ? t('transferring') : t('noTransfer') }}
          </div>
        </div>
        <button class="transfer-panel-close" @click="transferPanelOpen = false">×</button>
      </div>
      <div class="download-progress-text">
        {{ uploadProgress.completed }} / {{ uploadProgress.total }}
        <span v-if="uploadProgress.current"> · {{ uploadProgress.current }}</span>
      </div>
      <div class="transfer-progress-track">
        <div
          class="transfer-progress-bar"
          :style="{ width: `${uploadProgress.size ? Math.min(100, uploadProgress.bytes / uploadProgress.size * 100) : 0}%` }"
        ></div>
      </div>
      <div v-if="uploadProgress.size" class="transfer-progress-detail">
        {{ formatSize(uploadProgress.bytes) }} / {{ formatSize(uploadProgress.size) }}
        <span>{{ Math.min(100, Math.round(uploadProgress.bytes / uploadProgress.size * 100)) }}%</span>
      </div>
    </div>
  </div>
</template>
