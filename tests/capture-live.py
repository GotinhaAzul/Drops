#!/usr/bin/env python3
"""Opt-in live capture: own Foot/GTK windows, isolated daemon, local mock model only."""
import http.server,json,os,pathlib,signal,subprocess,tempfile,threading,time
ROOT=pathlib.Path(__file__).resolve().parents[1]
requests=[]
class Endpoint(http.server.BaseHTTPRequestHandler):
 def log_message(self,*a):pass
 def do_POST(self):
  requests.append(json.loads(self.rfile.read(int(self.headers['Content-Length']))))
  data=b'data: {"choices":[{"delta":{"content":"Capture received."},"finish_reason":"stop"}]}\n\ndata: [DONE]\n\n'
  self.send_response(200);self.send_header('Content-Type','text/event-stream');self.end_headers();self.wfile.write(data)
def wait(fn):
 deadline=time.monotonic()+20
 while time.monotonic()<deadline:
  value=fn()
  if value:return value
  time.sleep(.1)
 raise AssertionError('Timed out')
def main():
 instance=json.loads(subprocess.check_output(['hyprctl','instances','-j']))[0]
 env=dict(os.environ,HYPRLAND_INSTANCE_SIGNATURE=instance['instance'],WAYLAND_DISPLAY=instance['wl_socket'])
 assert not any('LOCK' in m.get('solitaryBlockedBy',[]) for m in json.loads(subprocess.check_output(['hyprctl','monitors','-j'],env=env))), 'Unlock the desktop first'
 server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Endpoint)
 threading.Thread(target=server.serve_forever,daemon=True).start()
 children=[]
 try:
  with tempfile.TemporaryDirectory(prefix='drops-capture-live-') as directory:
   tmp=pathlib.Path(directory);env['DROPS_SOCKET']=str(tmp/'drops.sock')
   cfg=tmp/'config.toml';cfg.write_text(f'[provider.local]\nbase_url="http://127.0.0.1:{server.server_port}/v1"\nmodel="mock"\nvision=true\ntools=false\n')
   with (tmp/'daemon.log').open('w+') as log:
    daemon=subprocess.Popen([str(ROOT/'target/debug/buddyd'),'--config',str(cfg),'--data-dir',str(tmp/'data')],env=env,stdout=log,stderr=log);children.append(daemon)
    def ctl(*args):return json.loads(subprocess.check_output([str(ROOT/'target/debug/buddyctl'),*args],env=env,text=True))
    wait(lambda:pathlib.Path(env['DROPS_SOCKET']).exists())
    for kind in ('terminal','editor','browser'):
     title=f'Drops capture {kind} {os.getpid()}'
     if kind=='terminal':
      command=['foot','--title',title,'sh','-c',"printf '\\n$ cargo bild\\nerror: no such command: bild\\nDid you mean build?\\n'; sleep 90"]
     elif kind=='browser':
      page=tmp/'page.html';page.write_text('<title>'+title+'</title><h1>Drops browser fixture</h1><p>The telescope found three small moons.</p>')
      command=['chromium','--ozone-platform=wayland','--force-renderer-accessibility','--no-first-run','--no-default-browser-check','--disable-background-networking','--disable-sync','--user-data-dir='+str(tmp/'browser-profile'),'--app='+page.as_uri()]
     else:
      code="import gi; gi.require_version('Gtk','3.0'); from gi.repository import Gtk; w=Gtk.Window(title="+repr(title)+"); v=Gtk.TextView(); v.get_buffer().set_text('def divide(a, b):\\n    return a / b\\n# Bug: b may be zero'); w.add(v); w.set_default_size(700,450); w.show_all(); Gtk.main()"
      command=['/usr/bin/python','-c',code]
     window=subprocess.Popen(command,env=env,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL);children.append(window)
     client=wait(lambda:next((c for c in json.loads(subprocess.check_output(['hyprctl','clients','-j'],env=env)) if c['title']==title),None));time.sleep(.7)
     x,y=client['at'];w,h=client['size'];blob=ctl('drop',str(x+w//2),str(y+h//2))['id']
     ctl('send',blob,'What went wrong in this window?')
     wait(lambda:next(b for b in ctl('status')['state']['blobs'] if b['id']==blob)['state']=='idle')
     context=ctl('context',blob)['context'];content=context['content']
     assert content['status']=='captured',content
     expected='bild' if kind=='terminal' else 'three small moons' if kind=='browser' else 'return a / b'
     assert expected in content['text'],content
     assert content['image']['data_url'].startswith('data:image/png;base64,')
     wire=requests[-1]['messages'];parts=[m['content'] for m in wire if isinstance(m.get('content'),list)]
     assert any(p.get('type')=='image_url' for arr in parts for p in arr),wire
     assert expected in json.dumps(wire)
     status=ctl('status');assert 'data:image/' not in json.dumps(status)
     if kind=='editor':assert any(s['kind']=='accessibility' for s in content['sources']),content
     print(f'PASS: {kind} visible text, image attachment, provenance, model request, compact state')
     ctl('remove',blob);window.terminate();window.wait(timeout=3);children.remove(window)
    ctl('configure',f'http://127.0.0.1:{server.server_port}/v1','mock','--vision','false','--capture-content','false','--screenshots','false')
    assert not ctl('status')['state']['connection']['capture_content']
    daemon.send_signal(signal.SIGINT);assert daemon.wait(timeout=5)==0;children.remove(daemon)
 finally:
  for child in reversed(children):
   if child.poll() is None:child.terminate();child.wait(timeout=5)
  server.shutdown();server.server_close()
if __name__=='__main__':main()
