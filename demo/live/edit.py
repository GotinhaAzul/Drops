#!/usr/bin/env python3
"""Edit real recorded Drops interactions into a 20-second feature demo."""
from PIL import Image,ImageDraw,ImageFont
import pathlib,json,subprocess,math,wave,struct,random
D=pathlib.Path(__file__).resolve().parent;TMP=D.parent.parent/'.test-data/live-demo';m=json.loads((D/'marks.json').read_text());TMP.mkdir(parents=True,exist_ok=True)
segments=[(m['create_start'],m['drag_start'],3),(m['drag_start'],m['prompt_start'],3),(m['prompt_start'],m['prompt_sent'],2.5),(m['prompt_sent'],m['answer_ready']+.3,2.5),(m['answer_ready']+.3,m['edit_start'],2),(m['edit_start'],m['approval_ready'],2.5),(m['approval_ready'],m['approved']+.1,3),(m['approved']+.1,m['end'],1.5)]
filters=['[0:v]split=8'+''.join(f'[s{i}]' for i in range(8))]
for i,(a,b,d) in enumerate(segments):filters.append(f'[s{i}]trim=start={a}:end={b},setpts=(PTS-STARTPTS)*{d/(b-a)},fps=24,tpad=stop_mode=clone:stop_duration=0.1,trim=duration={d},setpts=PTS-STARTPTS[v{i}]')
filters.append(''.join(f'[v{i}]' for i in range(8))+'concat=n=8:v=1:a=0[v]')
subprocess.run(['ffmpeg','-y','-v','error','-i',str(D/'raw-take.mp4'),'-filter_complex',';'.join(filters),'-map','[v]','-an','-c:v','libx264','-preset','fast','-crf','17',str(TMP/'timed.mp4')],check=True)
W,H,FPS=1920,1080,24
F='/usr/share/fonts/noto/NotoSans-'
fonts={n:ImageFont.truetype(F+('Bold' if b else 'Regular')+'.ttf',n) for n,b in [(20,False),(24,False),(28,False),(42,True),(26,True),(18,False)]}
# A quiet original ambient bed and small interaction accents. No stock media.
rate=48000;N=20*rate;rng=random.Random(14)
chords=[(130.8128,164.8138,195.9977),(110,130.8128,164.8138),(87.3071,130.8128,174.6141),(97.9989,146.8324,195.9977)]
clicks=[.8,2.6,3.7,5.3,7.9,11.8,17.8]
with wave.open(str(TMP/'sound.wav'),'wb') as f:
 f.setnchannels(2);f.setsampwidth(2);f.setframerate(rate)
 block=bytearray()
 for i in range(N):
  t=i/rate;ch=chords[min(3,int(t/5))];p=t%5;env=min(1,p/.7)*min(1,(5-p)/.8)*min(1,t/.8,(20-t)/.9)
  v=sum(math.sin(2*math.pi*q*t)*.012+math.sin(2*math.pi*q*2*t)*.003 for q in ch)*env
  for c in clicks:
   dt=t-c
   if 0<dt<.06:v+=(rng.random()*2-1)*.09*math.exp(-dt*95)
  val=max(-32768,min(32767,int(v*32767)));block.extend(struct.pack('<hh',val,val))
 f.writeframes(block)
# Camera deliberately leaves space for labels beside the active control.
keys=[(0,1.6,990,245),(2.8,1.75,1010,235),(3.7,1.2,850,330),(5.7,1.45,680,370),(6.4,1.72,580,350),(8.5,1.72,580,350),(9.4,1.85,600,405),(10.7,1.85,600,405),(11.6,1.72,580,355),(13,1.72,580,355),(14.7,1.58,575,300),(15.8,1.35,560,285),(17.8,1.35,560,285),(18.8,1.75,595,355),(20,1.75,595,355)]
def cam(t):
 for (a,z,x,y),(b,Z,X,Y) in zip(keys,keys[1:]):
  if a<=t<=b:
   q=(t-a)/(b-a);q=q*q*(3-2*q);return z+(Z-z)*q,x+(X-x)*q,y+(Y-y)*q
 return keys[-1][1:]
labels=[(0,3,'01  /  MAKE IT YOURS','Choose your companion.','Drop · Cat · Sprout'),(3,6,'02  /  PLACE IT','Drag. Drop. Ask.','A little companion, right where you work.'),(6,8.5,'03  /  ASK NATURALLY','Just type your request.','Real input in the native Drops interface.'),(8.5,11,'04  /  LOCAL AI','Gemma 4, running locally.','Real tools. A real streamed reply.'),(11,13,'05  /  FOLLOW THE THREAD','One click. Full context.','Compact replies + expandable reasoning.'),(13,15.5,'06  /  TAKE ACTION','Ask it to make a change.','File tools stay inside the workspace.'),(15.5,18.5,'07  /  STAY IN CONTROL','Review. Then approve.','See the diff before anything is written.'),(18.5,20,'DROPS  /  OPEN SOURCE','Done—with your go-ahead.','Your desktop, with a little help.')]
plates=[]
for a,b,k,title,sub in labels:
 p=Image.new('RGBA',(W,H));dr=ImageDraw.Draw(p)
 dr.rounded_rectangle((65,66,405,114),18,fill=(14,25,27,230));dr.text((86,76),'DROPS  /  LIVE DEMO',font=fonts[20],fill='#a7d8f5')
 dr.rounded_rectangle((65,785,936,1008),25,fill=(13,25,27,240),outline=(68,99,99,180),width=1)
 dr.rounded_rectangle((65,785,72,1008),3,fill='#a7d8f5')
 dr.text((98,810),k,font=fonts[20],fill='#a7d8f5')
 dr.text((96,854),title,font=fonts[42],fill='#f0f4ec')
 dr.text((98,925),sub,font=fonts[24],fill='#aabfb8')
 dr.text((1260,1030),'Live sandbox session · idle time condensed',font=fonts[18],fill='#8ba59c')
 plates.append(p)
dec=subprocess.Popen(['ffmpeg','-v','error','-i',str(TMP/'timed.mp4'),'-f','rawvideo','-pix_fmt','rgb24','-'],stdout=subprocess.PIPE)
enc=subprocess.Popen(['ffmpeg','-y','-v','error','-f','rawvideo','-pix_fmt','rgb24','-s',f'{W}x{H}','-r',str(FPS),'-i','-','-i',str(TMP/'sound.wav'),'-map','0:v','-map','1:a','-t','20','-c:v','libx264','-preset','fast','-crf','18','-pix_fmt','yuv420p','-c:a','aac','-b:a','160k','-movflags','+faststart',str(D.parent/'drops-live-demo.mp4')],stdin=subprocess.PIPE)
shots=[]
for n in range(20*FPS):
 buf=dec.stdout.read(1280*720*3)
 if len(buf)!=1280*720*3:raise RuntimeError('Decoded stream ended early')
 t=n/FPS;frame=Image.frombytes('RGB',(1280,720),buf);z,cx,cy=cam(t);cw,ch=1280/z,720/z
 x=max(0,min(1280-cw,cx-cw/2));y=max(0,min(720-ch,cy-ch/2))
 frame=frame.crop((int(x),int(y),int(x+cw),int(y+ch))).resize((W,H),Image.Resampling.BICUBIC).convert('RGBA')
 j=next(i for i,(a,b,*_) in enumerate(labels) if a<=t<b)
 a,b,*_=labels[j];alpha=min(1,(t-a)/.18)
 p=plates[j]
 if alpha<1:
  p=p.copy();p.putalpha(p.getchannel('A').point(lambda q:int(q*alpha)))
 # Brief rings emphasize the real click targets without replacing the cursor.
 for ct,px,py in [(0.6,1153,297),(2.5,1030,270),(5.3,760,510),(11.8,760,510),(18.05,660,388)]:
  dt=t-ct
  if 0<=dt<.45:
   halo=Image.new('RGBA',(W,H));hd=ImageDraw.Draw(halo);hx=(px-x)/cw*W;hy=(py-y)/ch*H;r=16+45*dt/.45
   hd.ellipse((hx-r,hy-r,hx+r,hy+r),outline=(167,216,245,int(210*(1-dt/.45))),width=3)
   frame.alpha_composite(halo)
 frame.alpha_composite(p)
 dr=ImageDraw.Draw(frame);dr.rounded_rectangle((65,1047,1855,1050),2,fill='#2a4142');dr.rounded_rectangle((65,1047,65+int(1790*t/20),1050),2,fill='#a7d8f5')
 if n in [48,120,174,244,294,354,414,470]:shots.append(frame.convert('RGB').resize((640,360)))
 if n==294:frame.convert('RGB').save(D.parent/'live-poster.jpg',quality=94)
 enc.stdin.write(frame.convert('RGB').tobytes())
enc.stdin.close();assert enc.wait()==0
dec.stdout.close();dec.wait()
contact=Image.new('RGB',(1280,1440),'#111c1e')
for i,s in enumerate(shots):contact.paste(s,((i%2)*640,(i//2)*360))
contact.save(TMP/'live-review.jpg',quality=94)
print(D.parent/'drops-live-demo.mp4')
