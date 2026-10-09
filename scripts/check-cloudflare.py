#!/usr/bin/env python3
"""Read-only check of the configured Cloudflare zone and DNS record; never prints tokens."""
import json
import os
from pathlib import Path
import re
import shlex
import sys
from urllib.error import HTTPError, URLError
from urllib.parse import urlencode, urlparse
from urllib.request import Request, urlopen

ROOT = Path(__file__).resolve().parent.parent
def config():
    values = {}
    path = ROOT / '.env'
    if path.exists():
        for line in path.read_text().splitlines():
            line = line.strip()
            if not line or line.startswith('#'):
                continue
            if line.startswith('export '):
                line = line[7:]
            key, sep, value = line.partition('=')
            if sep:
                parts = shlex.split(value, comments=True)
                values[key.strip()] = ' '.join(parts)
    values.update(os.environ)
    return values

def get(path, token):
    request = Request('https://api.cloudflare.com/client/v4' + path, headers={'Authorization': 'Bearer ' + token, 'Content-Type': 'application/json'})
    try:
        with urlopen(request, timeout=20) as response:
            data = json.load(response)
    except HTTPError as error:
        raise SystemExit(f'Cloudflare API HTTP {error.code}; 토큰과 Zone Read/DNS 권한을 확인하세요.') from None
    except URLError:
        raise SystemExit('Cloudflare API에 연결할 수 없습니다.') from None
    if not data.get('success'):
        codes = [str(error.get('code')) for error in data.get('errors', [])]
        raise SystemExit('Cloudflare API 오류 코드: ' + ', '.join(codes))
    return data['result']

def challenge_target(hostname):
    name = '_acme-challenge.' + hostname
    visited = set()
    for _ in range(8):
        if name in visited:
            raise SystemExit('ACME CNAME 위임에 순환 참조가 있습니다.')
        visited.add(name)
        request = Request('https://cloudflare-dns.com/dns-query?' + urlencode({'name':name,'type':'CNAME'}), headers={'Accept':'application/dns-json'})
        try:
            with urlopen(request, timeout=20) as response:
                data = json.load(response)
        except (HTTPError, URLError, ValueError):
            raise SystemExit('ACME CNAME 위임의 공개 DNS 조회에 실패했습니다.') from None
        if data.get('Status') not in (0,3):
            raise SystemExit('ACME CNAME 위임의 DNS 응답이 정상 상태가 아닙니다.')
        aliases = [entry['data'].rstrip('.').lower() for entry in data.get('Answer',[]) if entry['type']==5 and entry['name'].rstrip('.').lower()==name]
        if not aliases:
            return name
        if len(aliases) != 1:
            raise SystemExit('ACME CNAME 위임 대상이 하나가 아닙니다.')
        name = aliases[0]
    raise SystemExit('ACME CNAME 위임이 8단계를 초과합니다.')

def main():
    values = config()
    token = values.get('CF_API_TOKEN', '')
    zone_id = values.get('CF_ZONE_ID', '')
    if not token or not re.fullmatch(r'[a-fA-F0-9]{32}', zone_id):
        raise SystemExit('.env에 실제 CF_API_TOKEN과 CF_ZONE_ID(32자리 hex)가 필요합니다.')
    public = urlparse(values.get('P4FORGE_PUBLIC_URL', 'https://p4forge.zenogrid.co.kr:8444/'))
    hostname = public.hostname
    if not hostname or public.scheme != 'https':
        raise SystemExit('P4FORGE_PUBLIC_URL은 HTTPS 주소여야 합니다.')
    zone = get('/zones/' + zone_id, token)
    target = challenge_target(hostname)
    if target != zone['name'] and not target.endswith('.' + zone['name']):
        raise SystemExit('CF_ZONE_ID가 ACME CNAME 위임 대상의 zone과 일치하지 않습니다.')
    print(f"Cloudflare zone 확인: {zone['name']}; 인증서 도메인: {hostname}; challenge: {target}")
    records = get('/zones/' + zone_id + '/dns_records?' + urlencode({'name': hostname, 'per_page': 100}), token)
    address = [record for record in records if record['type'] in ('A','AAAA','CNAME')]
    delegated = not (hostname == zone['name'] or hostname.endswith('.' + zone['name']))
    if not address and not delegated:
        print('해당 호스트의 A/AAAA/CNAME 레코드가 없습니다. self-hosted 서버 주소를 DNS only로 등록해야 합니다.')
    if delegated:
        print('ACME CNAME 위임을 사용합니다. 서비스 A/AAAA 레코드는 원래 DNS zone에서 관리합니다.')
    for record in address:
        print(f"DNS {record['type']}: {'Proxied' if record.get('proxied') else 'DNS only'}")
    if public.port == 8444 and any(record.get('proxied') for record in address):
        raise SystemExit('8444는 Cloudflare 일반 프록시 지원 포트가 아닙니다. 해당 레코드를 DNS only로 변경하세요.')
    print('읽기 검증 완료. DNS 레코드는 변경하지 않았습니다. 인증서 발급에는 이 zone의 DNS Edit 권한도 필요합니다.')

if __name__ == '__main__':
    main()
