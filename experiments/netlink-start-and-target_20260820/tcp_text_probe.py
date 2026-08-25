import socket
held=[]
for _ in range(2):
    s=socket.socket(); s.bind(("127.0.0.1",0)); s.listen(1); held.append(s)
print(open("/proc/net/tcp").read().rstrip())
