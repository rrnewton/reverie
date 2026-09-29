#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/uio.h>
#include <sys/wait.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "captured-rights line=%d errno=%d check=%s\n", __LINE__, errno, #x); _exit(81); } } while (0)

static void send_rights(int socket, const int *fds, int count) {
  char byte = 'R';
  struct iovec iov = {&byte, 1};
  union { struct cmsghdr align; char bytes[CMSG_SPACE(2*sizeof(int))]; } control = {0};
  struct msghdr msg = {.msg_iov=&iov, .msg_iovlen=1, .msg_control=control.bytes,
    .msg_controllen=CMSG_SPACE(count*sizeof(int))};
  struct cmsghdr *h=CMSG_FIRSTHDR(&msg);
  h->cmsg_level=SOL_SOCKET; h->cmsg_type=SCM_RIGHTS; h->cmsg_len=CMSG_LEN(count*sizeof(int));
  memcpy(CMSG_DATA(h), fds, count*sizeof(int));
  CHECK(sendmsg(socket, &msg, 0)==1);
}

static void receive_rights(int socket, int *fds, int count, int flags) {
  char byte=0;
  struct iovec iov={&byte,1};
  union { struct cmsghdr align; char bytes[CMSG_SPACE(2*sizeof(int))]; } control={0};
  struct msghdr msg={.msg_iov=&iov,.msg_iovlen=1,.msg_control=control.bytes,.msg_controllen=sizeof(control.bytes)};
  CHECK(recvmsg(socket,&msg,flags|MSG_CMSG_CLOEXEC)==1 && byte=='R');
  struct cmsghdr *h=CMSG_FIRSTHDR(&msg);
  CHECK(h && h->cmsg_level==SOL_SOCKET && h->cmsg_type==SCM_RIGHTS);
  CHECK(h->cmsg_len==CMSG_LEN(count*sizeof(int)) && !(msg.msg_flags&MSG_CTRUNC));
  memcpy(fds,CMSG_DATA(h),count*sizeof(int));
  for(int i=0;i<count;i++) CHECK(fcntl(fds[i],F_GETFD)==FD_CLOEXEC);
}

static void captured_pipe(int fd, const struct stat *expected) {
  struct stat actual;
  CHECK(fstat(fd,&actual)==0 && S_ISFIFO(actual.st_mode));
  CHECK(actual.st_dev==expected->st_dev && actual.st_ino==expected->st_ino);
  unsigned char buffer[32]; memset(buffer,0xa5,sizeof(buffer));
  errno=0; CHECK(ioctl(fd,FIONREAD,buffer+8)==-1 && errno==ENOTTY);
  for(int i=0;i<32;i++) CHECK(buffer[i]==0xa5);
  errno=0; CHECK(read(fd,buffer,1)==-1 && errno==EBADF);
}

int main(void) {
  alarm(15);
  int pair[2]; CHECK(socketpair(AF_UNIX,SOCK_DGRAM,0,pair)==0);
  struct stat out,err; CHECK(fstat(1,&out)==0 && fstat(2,&err)==0);
  CHECK(out.st_ino!=err.st_ino);
  pid_t child=fork(); CHECK(child>=0);
  if(!child) {
    int fd=open("/proc/self/fd/1",O_WRONLY|O_NONBLOCK); CHECK(fd>=0);
    int alias=dup(fd); CHECK(alias>=0);
    int rights[2]={alias,2}; send_rights(pair[0],rights,2);
    CHECK(close(fd)==0 && close(alias)==0 && close(1)==0 && close(2)==0);
    _exit(0);
  }
  int status; CHECK(waitpid(child,&status,0)==child && WIFEXITED(status) && WEXITSTATUS(status)==0);
  int rights[2];
  receive_rights(pair[1],rights,2,MSG_PEEK);
  captured_pipe(rights[0],&out); captured_pipe(rights[1],&err);
  CHECK(fcntl(rights[0],F_GETFL)&O_NONBLOCK);
  CHECK(fcntl(rights[0],F_SETFL,O_APPEND)==0);
  CHECK(write(rights[0],"P",1)==1);
  CHECK(close(rights[0])==0 && close(rights[1])==0);
  receive_rights(pair[1],rights,2,MSG_PEEK);
  CHECK((fcntl(rights[0],F_GETFL)&(O_APPEND|O_NONBLOCK))==O_APPEND);
  CHECK(write(rights[0],"Q",1)==1);
  CHECK(close(rights[0])==0 && close(rights[1])==0);
  receive_rights(pair[1],rights,2,0);
  CHECK(write(rights[0],"R",1)==1 && write(rights[1],"E",1)==1);
  CHECK(close(rights[1])==0);
  struct iovec vectors[2]={{"V",1},{"W",1}};
  CHECK(writev(rights[0],vectors,2)==2);
  errno=0; CHECK(pwrite(rights[0],"bad",3,0)==-1 && errno==ESPIPE);
  CHECK(!(fcntl(1,F_GETFL)&O_APPEND)); /* reopen has independent OFD status */
  send_rights(pair[0],rights,1);
  CHECK(close(rights[0])==0);
  receive_rights(pair[1],rights,1,0);
  CHECK(write(rights[0],"Z",1)==1 && close(rights[0])==0);
  CHECK(close(pair[0])==0 && close(pair[1])==0);
  CHECK(write(1,"capture-rights-ok\n",18)==18);
  return 0;
}
