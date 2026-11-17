const messages = {
  'zh-CN': {
    language: '语言',
    followSoftware: '跟随软件语言',
    sharing: '共享中', folder: '共享目录', loading: '正在读取…', searchPlaceholder: '搜索文件名或目录名', search: '搜索',
    uploadFile: '↑ 上传文件', uploadFolder: '↑ 上传文件夹', transferProgress: '↕ 传输进度', inProgress: '进行中',
    sort: '排序规则', nameAsc: '文件名 A-Z', nameDesc: '文件名 Z-A', timeDesc: '时间 最新', timeAsc: '时间 最早',
    list: '☷ 列表', card: '▦ 卡片', refresh: '重新扫描并刷新当前目录', noMatches: '没有找到匹配内容', empty: '目录为空',
    folderItem: '文件夹', download: '下载', fullscreen: '全屏', close: '关闭', folderDownload: '下载文件夹',
    chooseFolder: '选择本地目录并保持原目录结构', archiveDownload: '打包下载', archiveDescription: 'ZIP 仅存储，不压缩，保持原目录结构', cancel: '取消',
    downloadingFolder: '正在下载文件夹', recentComplete: '最近一次传输已完成', transferring: '正在传输', noTransfer: '暂无进行中的传输',
    fileSharing: '文件共享', loadStatusFailed: '无法读取共享状态', loadFailed: '加载失败：', searchFailed: '搜索失败：', folderDownloadFailed: '文件夹下载失败：',
    useNodeSend: '使用 NodeSend 下载', nodeSendDownloadUnconfirmed: '未收到 NodeSend 下载任务确认，请检查软件是否已安装并查看传输记录。', saveImageAs: '图片另存为', copyImage: '复制图片', copyImageAddress: '复制图片地址',
    uploadFailed: '上传失败，可再次选择同一文件继续：', fileDownloadFailed: '下载失败，可再次选择同一文件继续：',
    imageCopyUnsupported: '当前浏览器不支持复制图片', folderPickerUnsupported: '当前浏览器不支持选择保存目录',
    localHttpFolderPicker: '当前通过局域网 HTTP 访问，浏览器无法选择本地保存目录；请使用“打包下载”',
    uploadNoProgress: '服务器未推进上传位置', resumeUnsupported: '服务器未提供断点下载支持', emptyResponse: '服务器返回空数据',
    createFolderFailed: '无法创建目录：', locateFolderFailed: '无法定位文件目录：', fileInfoFailed: '无法读取文件信息（HTTP ',
    imageConversionFailed: '图片转换失败', copyFailed: '复制失败', imageCopyFailed: '复制图片失败：', imageAddressCopyFailed: '复制图片地址失败：',
    imageSaveFailed: '图片另存为失败：', downloadHttpFailed: '下载失败：HTTP ',
  },
  'zh-TW': {
    language: '語言',
    followSoftware: '跟隨軟體語言',
    sharing: '分享中', folder: '分享目錄', loading: '正在讀取…', searchPlaceholder: '搜尋檔案名稱或目錄名稱', search: '搜尋',
    uploadFile: '↑ 上傳檔案', uploadFolder: '↑ 上傳資料夾', transferProgress: '↕ 傳輸進度', inProgress: '進行中',
    sort: '排序規則', nameAsc: '檔案名稱 A-Z', nameDesc: '檔案名稱 Z-A', timeDesc: '時間 最新', timeAsc: '時間 最早',
    list: '☷ 清單', card: '▦ 卡片', refresh: '重新掃描並重新整理目前目錄', noMatches: '找不到符合的內容', empty: '目錄是空的',
    folderItem: '資料夾', download: '下載', fullscreen: '全螢幕', close: '關閉', folderDownload: '下載資料夾',
    chooseFolder: '選擇本機目錄並保留原目錄結構', archiveDownload: '打包下載', archiveDescription: 'ZIP 僅儲存，不壓縮，保留原目錄結構', cancel: '取消',
    downloadingFolder: '正在下載資料夾', recentComplete: '最近一次傳輸已完成', transferring: '正在傳輸', noTransfer: '目前沒有進行中的傳輸',
    fileSharing: '檔案分享', loadStatusFailed: '無法讀取分享狀態', loadFailed: '載入失敗：', searchFailed: '搜尋失敗：', folderDownloadFailed: '資料夾下載失敗：',
    useNodeSend: '使用 NodeSend 下載', nodeSendDownloadUnconfirmed: '未收到 NodeSend 下載任務確認，請檢查軟體是否已安裝並查看傳輸記錄。', saveImageAs: '另存圖片', copyImage: '複製圖片', copyImageAddress: '複製圖片網址',
    uploadFailed: '上傳失敗，可再次選擇相同檔案繼續：', fileDownloadFailed: '下載失敗，可再次選擇相同檔案繼續：',
    imageCopyUnsupported: '目前瀏覽器不支援複製圖片', folderPickerUnsupported: '目前瀏覽器不支援選擇儲存目錄',
    localHttpFolderPicker: '目前透過區域網路 HTTP 存取，瀏覽器無法選擇本機儲存目錄；請使用「打包下載」',
    uploadNoProgress: '伺服器未推進上傳位置', resumeUnsupported: '伺服器未提供續傳支援', emptyResponse: '伺服器回傳空資料',
    createFolderFailed: '無法建立目錄：', locateFolderFailed: '無法定位檔案目錄：', fileInfoFailed: '無法讀取檔案資訊（HTTP ',
    imageConversionFailed: '圖片轉換失敗', copyFailed: '複製失敗', imageCopyFailed: '複製圖片失敗：', imageAddressCopyFailed: '複製圖片網址失敗：',
    imageSaveFailed: '另存圖片失敗：', downloadHttpFailed: '下載失敗：HTTP ',
  },
  'en-US': {
    language: 'Language',
    followSoftware: 'Follow app language',
    sharing: 'Sharing', folder: 'Shared folder', loading: 'Loading…', searchPlaceholder: 'Search files or folders', search: 'Search',
    uploadFile: '↑ Upload files', uploadFolder: '↑ Upload folder', transferProgress: '↕ Transfer progress', inProgress: 'In progress',
    sort: 'Sort order', nameAsc: 'Name A-Z', nameDesc: 'Name Z-A', timeDesc: 'Newest first', timeAsc: 'Oldest first',
    list: '☷ List', card: '▦ Cards', refresh: 'Rescan and refresh this folder', noMatches: 'No matching items found', empty: 'This folder is empty',
    folderItem: 'Folder', download: 'Download', fullscreen: 'Fullscreen', close: 'Close', folderDownload: 'Download folder',
    chooseFolder: 'Choose a local folder and keep the folder structure', archiveDownload: 'Download as ZIP', archiveDescription: 'Store files without compression and keep the folder structure', cancel: 'Cancel',
    downloadingFolder: 'Downloading folder', recentComplete: 'Most recent transfer completed', transferring: 'Transferring', noTransfer: 'No active transfers',
    fileSharing: 'File sharing', loadStatusFailed: 'Could not read share status', loadFailed: 'Loading failed: ', searchFailed: 'Search failed: ', folderDownloadFailed: 'Folder download failed: ',
    useNodeSend: 'Download with NodeSend', nodeSendDownloadUnconfirmed: 'No NodeSend download confirmation was received. Check that NodeSend is installed and review its transfer history.', saveImageAs: 'Save image as', copyImage: 'Copy image', copyImageAddress: 'Copy image address',
    uploadFailed: 'Upload failed. Select the same file again to resume: ', fileDownloadFailed: 'Download failed. Select the same file again to resume: ',
    imageCopyUnsupported: 'This browser does not support copying images', folderPickerUnsupported: 'This browser does not support choosing a save folder',
    localHttpFolderPicker: 'Browsers cannot choose a local folder over LAN HTTP. Use “Download as ZIP” instead.',
    uploadNoProgress: 'The server did not advance the upload position', resumeUnsupported: 'The server does not support resumable downloads', emptyResponse: 'The server returned no data',
    createFolderFailed: 'Could not create folder: ', locateFolderFailed: 'Could not locate the file folder: ', fileInfoFailed: 'Could not read file information (HTTP ',
    imageConversionFailed: 'Image conversion failed', copyFailed: 'Copy failed', imageCopyFailed: 'Could not copy image: ', imageAddressCopyFailed: 'Could not copy image address: ',
    imageSaveFailed: 'Could not save image: ', downloadHttpFailed: 'Download failed: HTTP ',
  },
  'ja-JP': {
    language: '言語',
    followSoftware: 'アプリの言語に従う',
    sharing: '共有中', folder: '共有フォルダー', loading: '読み込み中…', searchPlaceholder: 'ファイル名またはフォルダー名を検索', search: '検索',
    uploadFile: '↑ ファイルをアップロード', uploadFolder: '↑ フォルダーをアップロード', transferProgress: '↕ 転送の進行状況', inProgress: '進行中',
    sort: '並べ替え', nameAsc: '名前 A-Z', nameDesc: '名前 Z-A', timeDesc: '新しい順', timeAsc: '古い順',
    list: '☷ リスト', card: '▦ カード', refresh: '再スキャンして現在のフォルダーを更新', noMatches: '一致する項目がありません', empty: 'フォルダーは空です',
    folderItem: 'フォルダー', download: 'ダウンロード', fullscreen: '全画面表示', close: '閉じる', folderDownload: 'フォルダーをダウンロード',
    chooseFolder: '保存先フォルダーを選択して元の構成を維持', archiveDownload: 'ZIP でダウンロード', archiveDescription: '圧縮せずに ZIP に格納し、元のフォルダー構成を維持', cancel: 'キャンセル',
    downloadingFolder: 'フォルダーをダウンロード中', recentComplete: '直近の転送が完了しました', transferring: '転送中', noTransfer: '進行中の転送はありません',
    fileSharing: 'ファイル共有', loadStatusFailed: '共有状態を読み取れません', loadFailed: '読み込みに失敗しました：', searchFailed: '検索に失敗しました：', folderDownloadFailed: 'フォルダーのダウンロードに失敗しました：',
    useNodeSend: 'NodeSend でダウンロード', nodeSendDownloadUnconfirmed: 'NodeSend のダウンロード確認を受信できませんでした。ソフトウェアのインストールを確認し、転送履歴をご確認ください。', saveImageAs: '画像を名前を付けて保存', copyImage: '画像をコピー', copyImageAddress: '画像のアドレスをコピー',
    uploadFailed: 'アップロードに失敗しました。同じファイルを再選択して再開してください：', fileDownloadFailed: 'ダウンロードに失敗しました。同じファイルを再選択して再開してください：',
    imageCopyUnsupported: 'このブラウザーは画像のコピーに対応していません', folderPickerUnsupported: 'このブラウザーは保存先フォルダーの選択に対応していません',
    localHttpFolderPicker: 'LAN 内の HTTP アクセスでは保存先フォルダーを選択できません。「ZIP でダウンロード」を使用してください',
    uploadNoProgress: 'サーバーでアップロード位置が進みませんでした', resumeUnsupported: 'サーバーは再開ダウンロードに対応していません', emptyResponse: 'サーバーからデータが返されませんでした',
    createFolderFailed: 'フォルダーを作成できません：', locateFolderFailed: 'ファイルの保存先を特定できません：', fileInfoFailed: 'ファイル情報を読み取れません（HTTP ',
    imageConversionFailed: '画像を変換できません', copyFailed: 'コピーに失敗しました', imageCopyFailed: '画像をコピーできません：', imageAddressCopyFailed: '画像アドレスをコピーできません：',
    imageSaveFailed: '画像を保存できません：', downloadHttpFailed: 'ダウンロードに失敗しました：HTTP ',
  },
}

function normalizeLanguage(requested) {
  const value = String(requested || '').toLowerCase()
  return value.startsWith('zh-tw') || value.startsWith('zh-hant') ? 'zh-TW'
  : value.startsWith('en') ? 'en-US'
    : value.startsWith('ja') ? 'ja-JP' : 'zh-CN'
}

let language = normalizeLanguage(navigator.language || 'zh-CN')

export function t(key) {
  return messages[language][key] || messages['zh-CN'][key] || key
}

export function activeLanguage() {
  return language
}

export function setLanguage(requested) {
  const nextLanguage = normalizeLanguage(requested)
  if (language === nextLanguage) return
  language = nextLanguage
  document.documentElement.lang = nextLanguage
}
