import { activeLanguage, setLanguage, t } from './i18n.js'

const labels = {
  '共享中': 'sharing', '共享目录': 'folder', '正在读取…': 'loading', '搜索文件名或目录名': 'searchPlaceholder', '搜索': 'search',
  '↑ 上传文件': 'uploadFile', '↑ 上传文件夹': 'uploadFolder', '↕ 传输进度': 'transferProgress', '进行中': 'inProgress',
  '排序规则': 'sort', '文件名 A-Z': 'nameAsc', '文件名 Z-A': 'nameDesc', '时间 最新': 'timeDesc', '时间 最早': 'timeAsc',
  '☷ 列表': 'list', '▦ 列表': 'list', '列表': 'list', '▦ 卡片': 'card', '卡片': 'card', '文件夹': 'folderItem',
  '下载': 'download', '全屏': 'fullscreen', '关闭': 'close', '重新扫描并刷新当前目录': 'refresh', '正在加载…': 'loading',
  '没有找到匹配内容': 'noMatches', '目录为空': 'empty', '文件共享': 'fileSharing', '共享中': 'sharing',
  '无法读取共享状态': 'loadStatusFailed',
  '下载文件夹': 'folderDownload', '文件夹下载': 'folderDownload', '选择本地目录并保持原目录结构': 'chooseFolder',
  '打包下载': 'archiveDownload', 'ZIP 仅存储，不压缩，保持原目录结构': 'archiveDescription', '取消': 'cancel',
  '正在下载文件夹': 'downloadingFolder', '最近一次传输已完成': 'recentComplete', '正在传输': 'transferring', '暂无进行中的传输': 'noTransfer',
  '使用 NodeSend 下载': 'useNodeSend', '图片另存为': 'saveImageAs', '复制图片': 'copyImage', '复制图片地址': 'copyImageAddress',
  '当前浏览器不支持复制图片': 'imageCopyUnsupported',
  '当前浏览器不支持选择保存目录': 'folderPickerUnsupported', '服务器未推进上传位置': 'uploadNoProgress',
  '服务器未提供断点下载支持': 'resumeUnsupported', '服务器返回空数据': 'emptyResponse',
  '图片转换失败': 'imageConversionFailed', '复制失败': 'copyFailed',
  '当前通过局域网 HTTP 访问，浏览器无法选择本地保存目录；请使用“打包下载”': 'localHttpFolderPicker',
}

const prefixes = [
  ['文件夹下载失败：', 'folderDownloadFailed'], ['上传失败，可再次选择同一文件继续：', 'uploadFailed'],
  ['下载失败，可再次选择同一文件继续：', 'fileDownloadFailed'], ['加载失败：', 'loadFailed'], ['搜索失败：', 'searchFailed'],
  ['复制图片失败：', 'imageCopyFailed'], ['复制图片地址失败：', 'imageAddressCopyFailed'], ['图片另存为失败：', 'imageSaveFailed'],
  ['无法创建目录：', 'createFolderFailed'], ['无法定位文件目录：', 'locateFolderFailed'], ['无法读取文件信息（HTTP ', 'fileInfoFailed'],
  ['下载失败：HTTP ', 'downloadHttpFailed'], ['图片转换失败：', 'imageConversionFailed'],
]

function localized(value) {
  if (labels[value]) return t(labels[value])
  for (const [prefix, key] of prefixes) {
    if (value.startsWith(prefix)) return `${t(key)}${value.slice(prefix.length)}`
  }
  return value
}

function localizeNode(node) {
  if (node.nodeType === Node.TEXT_NODE) {
    const original = node.nodeValue || ''
    const trimmed = original.trim()
    const translated = localized(trimmed)
    if (translated !== trimmed) {
      const start = original.length - original.trimStart().length
      const end = original.length - original.trimEnd().length
      node.nodeValue = `${original.slice(0, start)}${translated}${end ? original.slice(-end) : ''}`
    }
    return
  }
  if (node.nodeType !== Node.ELEMENT_NODE) return
  const element = node
  for (const attribute of ['placeholder', 'title', 'aria-label', 'alt']) {
    const value = element.getAttribute(attribute)
    if (value) {
      const translated = localized(value)
      if (translated !== value) element.setAttribute(attribute, translated)
    }
  }
  for (const child of element.childNodes) localizeNode(child)
}

document.documentElement.lang = activeLanguage()
const localizePage = () => localizeNode(document.documentElement)
new MutationObserver(records => {
  for (const record of records) {
    if (record.type === 'childList') {
      for (const node of record.addedNodes) localizeNode(node)
    } else {
      localizeNode(record.target)
    }
  }
}).observe(document.documentElement, {
  subtree: true,
  childList: true,
  characterData: true,
  attributes: true,
  attributeFilter: ['placeholder', 'title', 'aria-label', 'alt'],
})
localizePage()
window.addEventListener('nodesend-language-changed', event => {
  setLanguage(event.detail)
  document.documentElement.lang = activeLanguage()
  localizePage()
})
