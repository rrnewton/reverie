#define _GNU_SOURCE
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
static volatile unsigned long sink[8];
static void *w(void *a){ long id=(long)a; unsigned long x=0;
  for(unsigned long i=0;i<20000000UL;i++){ x+=i^id; if((i&0xffff)==0) sink[id]=x; }
  sink[id]=x; return NULL; }
int main(int argc,char**argv){ int n=argc>1?atoi(argv[1]):6; pthread_t t[8];
  for(long i=0;i<n;i++) pthread_create(&t[i],NULL,w,(void*)i);
  for(int i=0;i<n;i++) pthread_join(t[i],NULL);
  unsigned long s=0; for(int i=0;i<n;i++) s+=sink[i]; printf("sum=%lu\n",s); return 0; }
