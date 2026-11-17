export function withAccessToken(path) {
  const token = new URLSearchParams(window.location.search).get('token')
  if (!token) return path

  const url = new URL(path, window.location.href)
  if (url.origin !== window.location.origin) return path
  url.searchParams.set('token', token)
  return `${url.pathname}${url.search}${url.hash}`
}
