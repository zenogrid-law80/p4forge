#!/usr/bin/env python3
"""Provision the development stream's restricted publisher; never print credentials."""
import json, os, pathlib, secrets, subprocess, tempfile
root = pathlib.Path(__file__).resolve().parents[1]
local = root / '.local'
local.mkdir(exist_ok=True)
port = 'p4forge.zenogrid.co.kr:1666'
user = 'p4forge-publisher'
paths = ['GameDesign/Tools/data_table/data_table/src', 'GameDesign/Output/Sqlite']
def docker(*args, input=None):
    return subprocess.run(['docker','compose','exec','-T','perforce',*args],cwd=root,input=input,stdout=subprocess.PIPE,stderr=subprocess.PIPE,check=True).stdout
with tempfile.TemporaryDirectory(prefix='publisher-admin-',dir=local) as td:
    env = dict(os.environ, P4TICKETS=str(pathlib.Path(td)/'tickets'),P4ENVIRO='/dev/null',P4CONFIG='',P4PASSWD='')
    def p4(*args, input=None, account='p4forge-admin', tickets=None):
        e = env.copy()
        if tickets: e['P4TICKETS']=str(tickets)
        return subprocess.run(['/opt/homebrew/bin/p4','-p',port,'-u',account,*args],cwd='/',env=e,input=input,stdout=subprocess.PIPE,stderr=subprocess.PIPE,check=True).stdout
    password = docker('cat','/data/.p4forge-credentials/admin-password')
    p4('login','-a',input=password)
    p4('user','-f','-i',input=f'User: {user}\nEmail: {user}@example.invalid\nFullName: P4Forge Generated Files Publisher\n'.encode())
    secret_path='/data/.p4forge-credentials/publisher-password'
    found=docker('sh','-c',f'test ! -f {secret_path} || cat {secret_path}')
    if not found:
        found=('P4forge!'+secrets.token_hex(24)+'\n').encode()
        p4('passwd',user,input=found+found)
        docker('sh','-c',f'umask 077; cat > {secret_path}',input=found)
    ready=docker('sh','-c','test ! -f /data/.p4forge-credentials/publisher-ready || printf ready')
    if not ready:
        fresh=('P4forge!'+secrets.token_hex(24)+'\n').encode()
        p4('passwd',input=found+fresh+fresh,account=user)
        found=fresh
        docker('sh','-c',f'umask 077; cat > {secret_path}; touch /data/.p4forge-credentials/publisher-ready',input=found)
    protections=p4('protect','-o').decode()
    # Append only missing exact rules, retaining every existing account's protections.
    rules=[f'read user {user} * //streams/root/...']+[f'write user {user} * //streams/root/{path}/...' for path in paths]
    existing={line.strip() for line in protections.splitlines()}
    for rule in rules:
        if rule not in existing: protections+='\n\t'+rule+'\n'
    p4('protect','-i',input=protections.encode())
    ticket=local/'publisher-tickets'
    fd=os.open(ticket,os.O_CREAT|os.O_WRONLY,0o600); os.close(fd); ticket.chmod(0o600)
    p4('login','-a',input=found,account=user,tickets=ticket)
    policy={'connection':{'port':port,'user':user},'server_id':'p4forge-dev','stream':'//streams/root','paths':paths,'tickets_file':'/Library/P4Forge/secrets/publisher-tickets'}
    out=local/'publisher.json';out.write_text(json.dumps(policy,indent=2)+'\n');out.chmod(0o600)
print('Restricted publisher ready; policy and ticket saved in .local (mode 600).')
