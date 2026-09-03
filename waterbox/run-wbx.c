#include "minibox.h"
#include <stdio.h>
#include <stdlib.h>
typedef struct { FILE *f; } freader;
static intptr_t file_read(uintptr_t ud, uint8_t *d, uintptr_t n){ return (intptr_t)fread(d,1,n,((freader*)ud)->f); }
static uintptr_t proc(mb_host*h,const char*n){ mb_return r; wbx_get_proc_addr(h,n,&r); if(r.error_message[0]){fprintf(stderr,"proc %s: %s\n",n,r.error_message);exit(2);} return r.data; }
int main(int argc,char**argv){
  FILE*f=fopen(argv[1],"rb"); if(!f){perror(argv[1]);return 1;}
  mb_memory_layout_template layout={16u<<20,16u<<20,16u<<20,16u<<20,64u<<20};
  freader fr={f}; mb_return r;
  wbx_create_host(&layout,"rustguest.wbx",file_read,(uintptr_t)&fr,&r); fclose(f);
  if(r.error_message[0]){fprintf(stderr,"create: %s\n",r.error_message);return 1;}
  mb_host*h=(mb_host*)r.data; wbx_activate_host(h,&r);
  int (*Init)(void)=(int(*)(void))proc(h,"Init");
  void (*FrameAdvance)(void)=(void(*)(void))proc(h,"FrameAdvance");
  unsigned long long (*GetDigest)(void)=(unsigned long long(*)(void))proc(h,"GetDigest");
  int ok=Init();
  unsigned long long d0=GetDigest();
  for(int i=0;i<5;i++) FrameAdvance();
  unsigned long long d5=GetDigest();
  printf("init=%d digest0=%016llx digest5=%016llx\n",ok,d0,d5);
  return 0;
}
