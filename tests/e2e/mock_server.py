import json, math, struct, io, sys, wave, hashlib, os
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse, parse_qs
from PIL import Image

PASSWORD="sesame"
ARTISTS=["Nina Simone","Daft Punk","Radiohead","Bonobo"]
SONGS=[]
for i in range(1,int(os.environ.get('MOCK_SONGS','40'))+1):
    SONGS.append({"id":str(i),"title":f"Track {i:02d} {['Alpha','Bravo','Charlie','Delta'][i%4]}","artist":f"{ARTISTS[i%4]} {i%137}",
      "album":f"Album {i%500}","albumId":f"al{i%5}","artistId":f"ar{i%4}","duration":20,"coverArt":f"al{i%5}","suffix":"wav","track":i,"year":2000+i%20})
LOG=[]
def wav(seconds=20):
    b=io.BytesIO(); w=wave.open(b,"wb"); w.setnchannels(1); w.setsampwidth(2); w.setframerate(22050)
    w.writeframes(b"".join(struct.pack("<h",int(8000*math.sin(2*math.pi*440*t/22050))) for t in range(22050*seconds))); w.close(); return b.getvalue()
WAV=wav()
def png(i):
    im=Image.new("RGB",(64,64),(30,30,30))
    for x in range(64):
        for y in range(64):
            im.putpixel((x,y),((x*4+i*50)%256,(y*4)%256,(200-i*30)%256))
    b=io.BytesIO(); im.save(b,"PNG"); return b.getvalue()
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
            d=png(int(q["id"][0][2:])); self.send_response(200); self.send_header("Content-Type","image/png"); self.send_header("Content-Length",str(len(d))); self.end_headers(); self.wfile.write(d)
        elif ep in("scrobble","star","unstar","setRating","updatePlaylist","createPlaylist","createBookmark","deletePlaylist"): self.send_json({})
        elif ep=="getBookmarks": self.send_json({"bookmarks":{}})
        elif ep=="createShare": self.send_json({"shares":{"share":[{"url":"https://example.test/share/abc"}]}})
        else: self.send_json({"status":"failed","error":{"code":70,"message":"not found"}})
    def _fail(self):
        d=json.dumps({"subsonic-response":{"status":"failed","version":"1.16.1","error":{"code":40,"message":"Wrong username or password"}}}).encode()
        self.send_response(200); self.send_header("Content-Type","application/json"); self.send_header("Content-Length",str(len(d))); self.end_headers(); self.wfile.write(d)
ThreadingHTTPServer(("127.0.0.1",int(sys.argv[1])),H).serve_forever()
