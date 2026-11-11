const CACHE = "monica-rust-shell-v1";
self.addEventListener("install",event=>{event.waitUntil(caches.open(CACHE).then(cache=>cache.addAll(["/","/manifest.webmanifest"])));self.skipWaiting();});
self.addEventListener("activate",event=>{event.waitUntil(caches.keys().then(keys=>Promise.all(keys.filter(key=>key.startsWith("monica-rust-shell-")&&key!==CACHE).map(key=>caches.delete(key)))).then(()=>self.clients.claim()));});
self.addEventListener("fetch",event=>{
 const url=new URL(event.request.url);
 if(event.request.method!=="GET"||url.origin!==self.location.origin||url.pathname.startsWith("/api/")||url.pathname.startsWith("/internal/"))return;
 const navigation=event.request.mode==="navigate";
 const staticAsset=url.pathname.startsWith("/assets/")||url.pathname.startsWith("/vendor/");
 if(!navigation&&!staticAsset)return;
 event.respondWith(fetch(event.request).then(response=>{
  if(response.ok&&staticAsset){const copy=response.clone();event.waitUntil(caches.open(CACHE).then(cache=>cache.put(event.request,copy)));}
  return response;
 }).catch(async()=>{const cached=await caches.match(navigation?"/":event.request);return cached||new Response("Offline",{status:503});}));
});
