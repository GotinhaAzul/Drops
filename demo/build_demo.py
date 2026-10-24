#!/usr/bin/env python3
"""Build a 20s product demo from native QML renders of a live Gemma session."""
from PIL import Image,ImageDraw,ImageFont,ImageFilter
import pathlib,subprocess,math
D=pathlib.Path(__file__).resolve().parent
W,H,FPS=1920,1080,24
BG='#111b1c'; WHITE='#edf2e9'; MUTED='#9fb3ac'; BLUE='#a7d8f5'; GREEN='#b8dca4'
def font(n,b=False):return ImageFont.truetype('/usr/share/fonts/noto/NotoSans-'+('Bold' if b else 'Regular')+'.ttf',n)
def text(im,xy,s,n=30,c=WHITE,b=False):ImageDraw.Draw(im).text(xy,s,font=font(n,b),fill=c,spacing=12)
def rr(im,box,c='#1d2b2b',r=25,outline=None):ImageDraw.Draw(im).rounded_rectangle(box,r,fill=c,outline=outline,width=2)
def asset(name):return Image.open(D/'assets'/name).convert('RGBA')
def paste(im,a,x,y,scale=1):
 a=a.resize((int(a.width*scale),int(a.height*scale)),Image.Resampling.LANCZOS)
 shadow=Image.new('RGBA',im.size);shadow.paste(a,(int(x),int(y)+15),a);shadow.putalpha(shadow.getchannel('A').point(lambda x:int(x*.35)));im.alpha_composite(shadow.filter(ImageFilter.GaussianBlur(20)))
 im.alpha_composite(a,(int(x),int(y)))
def base(i,label):
 im=Image.new('RGBA',(W,H),BG)
 draw=ImageDraw.Draw(im)
 for r in range(600,0,-12):draw.ellipse((1450-r,450-r,1450+r,450+r),fill=(17+int((600-r)/45),29+int((600-r)/25),30+int((600-r)/28),255))
 text(im,(100,60),'drops',34,BLUE,True);text(im,(1580,68),'OPEN SOURCE',20,MUTED,True)
 text(im,(100,976),label,21,MUTED);text(im,(1710,976),f'0{i} / 06',20,MUTED)
 return im
panels=[asset(x+'-panel.png') for x in ['01-panel','02-cat','03-sprout']]
faces=[a.crop((83,32,178,119)) for a in panels]
slides=[]
im=base(1,'A small companion. A capable assistant.')
text(im,(100,255),'Meet Drops.',108,WHITE,True)
text(im,(107,423),'AI that lives on your desktop.',43,MUTED)
rr(im,(108,550,620,615),'#253b3c',20);text(im,(131,563),'Hyprland  /  Omarchy  /  MIT',25,BLUE)
for j,a in enumerate(faces):paste(im,a,1070+j*215,410+(j%2)*55,2.15)
slides.append((3,im))
im=base(2,'Native Drops interface • isolated demo workspace')
text(im,(100,230),'Pick a personality.\nPlace it anywhere.',72,WHITE,True)
text(im,(105,470),'Drop, Cat or Sprout. Up to three companions.',31,MUTED)
text(im,(105,570),'Window + workspace anchors',32,BLUE,True)
text(im,(105,630),'Context, screenshots and OCR',30,MUTED)
for j,a in enumerate(panels):
 # Native avatar portion, with independent colored card.
 rr(im,(1000+j*255,250,1225+j*255,610),'#243332')
 paste(im,faces[j],1020+j*255,300,1.9)
 text(im,(1040+j*255,525),['Drop','Cat','Sprout'][j],28,[BLUE,'#f2c6a0',GREEN][j],True)
text(im,(1080,700),'They stay with your context.',30,MUTED)
slides.append((3,im))
im=base(3,'Live Gemma 4 session • native UI render • response time condensed')
text(im,(100,235),'Ask. Read.\nUnderstand.',83,WHITE,True)
text(im,(105,492),'Gemma 4, running locally with localai.',32,BLUE)
text(im,(105,580),'Real README → real tool call → real answer.',29,MUTED)
rr(im,(108,700,770,768),'#233b32',18);text(im,(132,715),'Local or hosted · OpenAI-compatible',27,GREEN)
paste(im,asset('05-chat-chat.png'),1180,235,2.3)
slides.append((4,im))
im=base(4,'Live file approval • confirmed output: “Ready to ship with Drops.”')
text(im,(100,220),'Review the diff.\nYou decide.',78,WHITE,True)
text(im,(105,456),'File changes wait for your approval.',32,MUTED)
rr(im,(105,555,940,760),'#20332e',22)
text(im,(135,578),'+++ launch-note.txt',28,MUTED)
text(im,(135,635),'+ Ready to ship with Drops.',35,GREEN)
text(im,(108,810),'Confined tools · backups · stale-file checks',27,BLUE)
paste(im,asset('06-approval-chat.png'),1220,160,1.8)
slides.append((4,im))
im=base(5,'Capabilities overview • availability depends on anchor and model')
text(im,(100,180),'Small companion. Full toolkit.',67,WHITE,True)
items=[('EXPLORE','Read, search, web + saved Office files'),('CONVERSE','Streaming chat + expandable reasoning'),('REMEMBER','Task memory + independent sessions'),('CONTROL','Fork, export, clear + cancel')]
for j,(title,body) in enumerate(items):
 x=100+(j%2)*870;y=340+(j//2)*220
 rr(im,(x,y,x+820,y+175),'#203030',24)
 text(im,(x+30,y+25),title,24,BLUE,True);text(im,(x+30,y+82),body,29,WHITE)
text(im,(108,845),'Bring your own model. Keep local requests local.',31,GREEN)
slides.append((4,im))
im=base(6,'Open source • MIT licensed')
for j,a in enumerate(faces):paste(im,a,747+j*150,190,1.4)
text(im,(635,390),'Your desktop,',81,WHITE,True)
text(im,(610,500),'with a little help.',81,WHITE,True)
rr(im,(700,690,1220,775),'#a7d8f5',28);text(im,(783,704),'Meet Drops.',44,'#172a2b',True)
slides.append((2,im))
# Static scenes with a gentle camera move and short fades, encoded directly.
p=subprocess.Popen(['ffmpeg','-y','-loglevel','error','-f','rawvideo','-pix_fmt','rgb24','-s',f'{W}x{H}','-r',str(FPS),'-i','-','-an','-c:v','libx264','-preset','fast','-crf','19','-pix_fmt','yuv420p','-movflags','+faststart',str(D/'drops-demo.mp4')],stdin=subprocess.PIPE)
prev=None
for duration,slide in slides:
 slide=slide.convert('RGB');n=duration*FPS
 for f in range(n):
  z=1+0.012*f/max(n-1,1);sw,sh=int(W/z),int(H/z)
  frame=slide.crop(((W-sw)//2,(H-sh)//2,(W+sw)//2,(H+sh)//2)).resize((W,H),Image.Resampling.BICUBIC)
  if prev is not None and f<7:frame=Image.blend(prev,frame,(f+1)/7)
  p.stdin.write(frame.tobytes())
 prev=frame
p.stdin.close();assert p.wait()==0
slides[2][1].convert('RGB').save(D/'poster.jpg',quality=93)
# Contact sheet for editorial verification.
contact=Image.new('RGB',(1440,540),'#111b1c')
for i,(_,s) in enumerate(slides):contact.paste(s.convert('RGB').resize((480,270)),((i%3)*480,(i//3)*270))
contact.save(D/'storyboard.jpg',quality=92)
print(D/'drops-demo.mp4')
