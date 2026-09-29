#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <unistd.h>
#define CHECK(x) do { if (!(x)) { fprintf(stderr,"closed-stdio line=%d errno=%d check=%s\n",__LINE__,errno,#x); _exit(83); } } while(0)
int main(int argc, char **argv) {
  alarm(15);
  CHECK(argc == 6 && strcmp(argv[1], "stdin") == 0);
  struct stat before, received;
  CHECK(fstat(0,&before) == 0);
  CHECK((uintmax_t)before.st_dev == strtoumax(argv[3],NULL,10));
  CHECK((uintmax_t)before.st_ino == strtoumax(argv[4],NULL,10));
  CHECK(S_ISCHR(before.st_mode));
  int flags=fcntl(0,F_GETFL); CHECK(flags>=0 && (flags&O_ACCMODE)==O_RDONLY);
  int pair[2]; CHECK(socketpair(AF_UNIX,SOCK_DGRAM,0,pair)==0);
  char byte='R'; struct iovec iov={&byte,1};
  union { struct cmsghdr align; char bytes[CMSG_SPACE(sizeof(int))]; } control={0};
  struct msghdr message={.msg_iov=&iov,.msg_iovlen=1,.msg_control=control.bytes,.msg_controllen=sizeof(control.bytes)};
  struct cmsghdr *header=CMSG_FIRSTHDR(&message);
  header->cmsg_level=SOL_SOCKET; header->cmsg_type=SCM_RIGHTS; header->cmsg_len=CMSG_LEN(sizeof(int));
  int donor=0; memcpy(CMSG_DATA(header),&donor,sizeof(donor));
  CHECK(sendmsg(pair[0],&message,0)==1);
  memset(&control,0xa5,sizeof(control)); byte=0;
  CHECK(recvmsg(pair[1],&message,MSG_CMSG_CLOEXEC)==1 && byte=='R');
  CHECK(!(message.msg_flags&(MSG_CTRUNC|MSG_TRUNC)) && message.msg_controllen==CMSG_SPACE(sizeof(int)));
  header=CMSG_FIRSTHDR(&message);
  CHECK(header && header->cmsg_level==SOL_SOCKET && header->cmsg_type==SCM_RIGHTS);
  CHECK(header->cmsg_len==CMSG_LEN(sizeof(int)) && CMSG_NXTHDR(&message,header)==NULL);
  int fd=-1; memcpy(&fd,CMSG_DATA(header),sizeof(fd));
  CHECK(fd>2 && fcntl(fd,F_GETFD)==FD_CLOEXEC && fstat(fd,&received)==0);
  CHECK(received.st_dev==before.st_dev && received.st_ino==before.st_ino && received.st_mode==before.st_mode);
  CHECK(fcntl(fd,F_SETFL,flags^O_NONBLOCK)==0 && fcntl(0,F_GETFL)==(flags^O_NONBLOCK));
  CHECK(fcntl(fd,F_SETFL,flags)==0 && fcntl(0,F_GETFL)==flags);
  CHECK(close(0)==0); byte='Q'; CHECK(read(fd,&byte,1)==0 && byte=='Q');
  CHECK(close(fd)==0 && close(pair[0])==0 && close(pair[1])==0);
  const char done[]="closed-stdio-default-input-rights-ok\n";
  CHECK(write(atoi(argv[5]),done,sizeof(done)-1)==sizeof(done)-1);
  return 0;
}
