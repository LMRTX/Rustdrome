import json, math, struct, io, sys, wave, hashlib, os
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse, parse_qs
from PIL import Image, ImageFilter
import random, time

PASSWORD="sesame"
COVERS=int(os.environ.get('MOCK_COVERS','5'))          # distinct albums/covers
LATENCY=float(os.environ.get('MOCK_LATENCY_MS','0'))/1000   # simulated network latency on cover requests
_COVER_CACHE={}
ARTISTS=["Nina Simone","Daft Punk","Radiohead","Bonobo"]
SONGS=[]
for i in range(1,int(os.environ.get('MOCK_SONGS','40'))+1):
    SONGS.append({"id":str(i),"title":f"Track {i:02d} {['Alpha','Bravo','Charlie','Delta'][i%4]}","artist":f"{ARTISTS[i%4]} {i%137}",
      "album":f"Album {i%500}","albumId":f"al{i%COVERS}","artistId":f"ar{i%4}","duration":(int(os.environ.get("MOCK_WAV_SECONDS","0")) or 20),"coverArt":f"al{i%COVERS}","suffix":"wav","track":i,"year":2000+i%20})
LOG=[]
def wav(seconds=20):
    b=io.BytesIO(); w=wave.open(b,"wb"); w.setnchannels(1); w.setsampwidth(2); w.setframerate(22050)
    w.writeframes(b"".join(struct.pack("<h",int(8000*math.sin(2*math.pi*440*t/22050))) for t in range(22050*seconds))); w.close(); return b.getvalue()
def big_wav(seconds):
    """A real-sized lossless track (44.1 kHz stereo 16-bit ≈ 10 MB/min) of noise — only its size matters."""
    n=seconds*44100*4; b=io.BytesIO(); w=wave.open(b,"wb"); w.setnchannels(2); w.setsampwidth(2); w.setframerate(44100)
    w.writeframes(os.urandom(n)); w.close(); return b.getvalue()
SECONDS=int(os.environ.get("MOCK_WAV_SECONDS","0"))
WAV=big_wav(SECONDS) if SECONDS else wav()
def png(i, size=640):
    """A photo-ish cover: smooth gradient, soft blobs and film grain, as a JPEG (like real artwork)."""
    key=(i,size)
    if key in _COVER_CACHE: return _COVER_CACHE[key]
    rnd=random.Random(i)
    base=Image.new("RGB",(size//4,size//4))
    px=base.load()
    c1=(rnd.randrange(256),rnd.randrange(256),rnd.randrange(256)); c2=(rnd.randrange(256),rnd.randrange(256),rnd.randrange(256))
    for y in range(size//4):
        for x in range(size//4):
            t=(x+y)/(size/2)
            px[x,y]=tuple(int(c1[k]*(1-t)+c2[k]*t) for k in range(3))
    for _ in range(14):
        cx,cy,r=rnd.randrange(size//4),rnd.randrange(size//4),rnd.randrange(8,size//8)
        col=(rnd.randrange(256),rnd.randrange(256),rnd.randrange(256))
        for y in range(max(0,cy-r),min(size//4,cy+r)):
            for x in range(max(0,cx-r),min(size//4,cx+r)):
                if (x-cx)**2+(y-cy)**2<r*r: px[x,y]=tuple((px[x,y][k]*2+col[k])//3 for k in range(3))
    im=base.resize((size,size),Image.BICUBIC).filter(ImageFilter.GaussianBlur(1))
    noise=Image.effect_noise((size,size),18).convert("RGB")
    im=Image.blend(im,noise,0.16)
    b=io.BytesIO(); im.save(b,"JPEG",quality=85); data=b.getvalue()
    _COVER_CACHE[key]=data; return data
class H(BaseHTTPRequestHandler):
    def log_message(self,*a): pass
    def send_json(self,body):
        d=json.dumps({"subsonic-response":{"status":"ok","version":"1.16.1",**body}}).encode()
        self.send_response(200); self.send_header("Content-Type","application/json"); self.send_header("Content-Length",str(len(d))); self.end_headers(); self.wfile.write(d)
    def do_GET(self):
        u=urlparse(self.path); q=parse_qs(u.query); ep=u.path.rsplit("/",1)[-1]
        LOG.append((ep,{k:v for k,v in q.items() if k not in("u","t","s","v","c","f")}))
        open("/tmp/mock_requests.log","a").write(json.dumps([ep,{k:v for k,v in q.items() if k not in("u","t","s","v","c","f")}])+"\n")
        # auth check
        t=q.get("t",[""])[0]; s=q.get("s",[""])[0]
        if hashlib.md5((PASSWORD+s).encode()).hexdigest()!=t:
            return self.send_json({"status":"failed","error":{"code":40,"message":"Wrong username or password"}}) if False else self._fail()
        if ep=="ping": self.send_json({"type":"navidrome","serverVersion":"0.53"})
        elif ep=="search3":
            qs=q.get("query",[""])[0].strip('"').lower()
            off=int(q.get("songOffset",["0"])[0]); cnt=int(q.get("songCount",["20"])[0])
            hits=[s for s in SONGS if qs in (s["title"]+s["artist"]).lower()]
            res={"song":hits[off:off+cnt]}
            if int(q.get("albumCount",["0"])[0]): res["album"]=[{"id":"al1","name":"Album 1","artist":"Daft Punk","year":2001,"songCount":8}] if qs else []
            if int(q.get("artistCount",["0"])[0]): res["artist"]=[{"id":"ar1","name":"Daft Punk","albumCount":3}] if "daft" in qs else []
            self.send_json({"searchResult3":res})
        elif ep=="getAlbumList2": self.send_json({"albumList2":{"album":[{"id":f"al{i}","name":f"Album {i}"} for i in range(3)]}})
        elif ep=="getAlbum":
            aid=q["id"][0]; self.send_json({"album":{"id":aid,"song":[s for s in SONGS if s["albumId"]==aid]}})
        elif ep=="getPlaylists": self.send_json({"playlists":{"playlist":[{"id":"p1","name":"Road trip","songCount":3}]}})
        elif ep=="getPlaylist": self.send_json({"playlist":{"id":"p1","entry":SONGS[:3]}})
        elif ep=="getStarred2": self.send_json({"starred2":{"song":[dict(SONGS[0],starred="2024-01-01")]}})
        elif ep=="getGenres": self.send_json({"genres":{"genre":[{"value":"Jazz","songCount":10},{"value":"Trip-hop","songCount":30}]}})
        elif ep=="getSongsByGenre": self.send_json({"songsByGenre":{"song":SONGS[:5]}})
        elif ep=="getInternetRadioStations": self.send_json({"internetRadioStations":{"internetRadioStation":[]}})
        elif ep=="getPodcasts": self.send_json({"podcasts":{"channel":[]}})
        elif ep=="getLyricsBySongId":
            self.send_json({"lyricsList":{"structuredLyrics":[{"synced":True,"line":[{"start":0,"value":"first lyric line"},{"start":3000,"value":"second lyric line"},{"start":9000,"value":"third lyric line"}]}]}})
        elif ep in("stream","download"):
            self.send_response(200); self.send_header("Content-Type","audio/wav"); self.send_header("Content-Length",str(len(WAV))); self.end_headers(); self.wfile.write(WAV)
        elif ep=="getCoverArt":
            if LATENCY: time.sleep(LATENCY)
            d=png(int(q["id"][0][2:]), int(q.get("size",["640"])[0])); self.send_response(200); self.send_header("Content-Type","image/jpeg"); self.send_header("Content-Length",str(len(d))); self.end_headers(); self.wfile.write(d)
        elif ep in("scrobble","star","unstar","setRating","updatePlaylist","createPlaylist","createBookmark","deletePlaylist"): self.send_json({})
        elif ep=="getBookmarks": self.send_json({"bookmarks":{}})
        elif ep=="createShare": self.send_json({"shares":{"share":[{"url":"https://example.test/share/abc"}]}})
        else: self.send_json({"status":"failed","error":{"code":70,"message":"not found"}})
    def _fail(self):
        d=json.dumps({"subsonic-response":{"status":"failed","version":"1.16.1","error":{"code":40,"message":"Wrong username or password"}}}).encode()
        self.send_response(200); self.send_header("Content-Type","application/json"); self.send_header("Content-Length",str(len(d))); self.end_headers(); self.wfile.write(d)
# warm the cover cache so measurements see the app's cost, not this script generating images
for _i in range(min(COVERS, 64)): png(_i, 640)
ThreadingHTTPServer(("127.0.0.1",int(sys.argv[1])),H).serve_forever()
