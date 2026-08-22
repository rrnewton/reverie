import socket, struct, hashlib, sys
# Generate loopback traffic first.
srv = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); srv.bind(("127.0.0.1", 0))
cli = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
for _ in range(int(sys.argv[1])):
    cli.sendto(b"x"*512, srv.getsockname()); srv.recv(4096)
cli.close(); srv.close()
# Now dump RTM_GETLINK and report IFLA_STATS64 on lo.
s = socket.socket(socket.AF_NETLINK, socket.SOCK_RAW, 0); s.bind((0,0))
s.send(struct.pack("=IHHII",20,18,0x001|0x300,1,0)+struct.pack("=Bxxx",socket.AF_PACKET))
blob=b""
while True:
    d=s.recv(65536); blob+=d
    if len(d)>=6 and struct.unpack("=H",d[4:6])[0]==3: break
s.close()
off=32; end=struct.unpack("<I",blob[0:4])[0]
while off+4<=end:
    l,t=struct.unpack("<HH",blob[off:off+4])
    if l<4: break
    if t==23:
        v=struct.unpack("<"+"Q"*25, blob[off+4:off+4+200])
        print("IFLA_STATS64 rx_packets=%d tx_packets=%d rx_bytes=%d tx_bytes=%d" % (v[0],v[1],v[2],v[3]))
    off+=(l+3)&~3
print("total_bytes",len(blob),"sha256",hashlib.sha256(blob).hexdigest())
import sys as _s
if "--hex" in _s.argv: print(blob.hex())
