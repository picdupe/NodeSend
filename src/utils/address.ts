// Prefer IPv4, then private/local addresses within each address family.
export function addressPriority(address: string): number {
  if (!address.includes(':')) {
    const [first, second] = address.split('.').map(Number);
    const privateAddress = first === 10 || (first === 172 && second >= 16 && second <= 31) || (first === 192 && second === 168);
    return privateAddress ? 0 : 1;
  }
  const ipv6 = address.replace(/^\[/, '').toLowerCase();
  return /^(?:f[cd][0-9a-f]{2}|fe[89ab][0-9a-f]):/.test(ipv6) ? 2 : 3;
}

