#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
int main(void){
    char buf[4096];
    const char *p = getenv("PWD");
    printf("CPROBE getcwd=%s\n", getcwd(buf,sizeof buf) ? buf : "?");
    printf("CPROBE PWD_env=%s\n", p ? p : "<unset>");
    return 0;
}
