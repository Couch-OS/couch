/* Small fbdev status screen. Reads whitelisted RAM events, never storage/logs.
 * Pixel layout and 4096-byte panel writes follow src/fbcon.c. */
#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
#include <string.h>
#include <unistd.h>
#include <fcntl.h>
#include <sys/ioctl.h>
#include <linux/fb.h>
#include "display_assets.h"

#define W 480
#define H 800
static unsigned char rgb[W * H * 3];
struct event { int phase, target, wifi, error; unsigned long long done, total, rate; };
static const char *phases[] = {"wait","connect","download","verify","write","complete","error","backup"};
static const char *targets[] = {"none","boot","recovery","userdata","logo","odmdtbo","ram","proinfo","nvram","nvdata","protect1","protect2"};
static const char *networks[] = {"waiting","ready","connecting","connected","failed"};
static const char *errors[] = {"none","network","verify","storage","protocol"};
static int lookup(const char *word, const char **table, int count) {
    for (int i = 0; i < count; i++) if (!strcmp(word, table[i])) return i;
    return -1;
}
static int parse(const char *line, struct event *event) {
    char version[8], phase[16], target[16], wifi[16], error[16], extra;
    struct event value = {0};
    if (sscanf(line, "%7s %15s %15s %llu %llu %llu %15s %15s %c", version, phase, target,
        &value.done, &value.total, &value.rate, wifi, error, &extra) != 8 || strcmp(version,"v1")) return 0;
    value.phase = lookup(phase, phases, 8); value.target = lookup(target, targets, 12);
    value.wifi = lookup(wifi, networks, 5); value.error = lookup(error, errors, 5);
    if (value.phase < 0 || value.target < 0 || value.wifi < 0 || value.error < 0 ||
        value.done > value.total || value.total > 128ull*1024*1024*1024 || value.rate > 128ull*1024*1024*1024) return 0;
    *event = value; return 1;
}
static void pixel(int x, int y, int r, int g, int b, int a) {
    if (x < 0 || x >= W || y < 0 || y >= H) return;
    unsigned char *p = rgb + (y * W + x) * 3;
    p[0] = (p[0]*(255-a)+r*a)/255; p[1] = (p[1]*(255-a)+g*a)/255; p[2] = (p[2]*(255-a)+b*a)/255;
}
static void box(int x, int y, int w, int h, int r, int g, int b) {
    for (int j=y;j<y+h;j++) for(int i=x;i<x+w;i++) pixel(i,j,r,g,b,255);
}
static void text(int x, int y, const char *s, int shade) {
    while (*s) {
        unsigned char ch = (unsigned char)*s++;
        if (ch < 32 || ch > 126) continue;
        if (x + font_advance[ch-32] > W-32) break;
        for (int j=0;j<40;j++) for(int i=0;i<32;i++)
            pixel(x+i,y+j,shade,shade,shade,font_alpha[j*32*96+(ch-32)*32+i]);
        x += font_advance[ch-32];
    }
}
static void render(const struct event *e) {
    box(0,0,W,H,9,9,11);
    for (int y=0;y<WORDMARK_H;y++) for(int x=0;x<WORDMARK_W;x++)
        pixel(32+x,48+y,245,245,247,wordmark_alpha[y*WORDMARK_W+x]);
    text(32,130,"INSTALLER",145);
    box(32,190,416,1,48,48,54);
    const char *title[] = {"Waiting for computer","Connecting to WiFi","Transferring data",
        "Verifying data","Writing system","Transfer complete","Stopped","Backing up data"};
    text(32,226,title[e->phase],245);
    const char *detail[] = {"USB bootstrap is ready","Connect using the installer","Secure WiFi transfer",
        "Checking SHA-256","Keep the remote connected","Check the computer to continue","Check the computer for details","Keeping the original data"};
    text(32,276,detail[e->phase],155);
    if (e->target) {
        char label[64];
        snprintf(label,sizeof label,"%s%s", e->target == 6 ? "" : "Partition: ", e->target == 6 ? "RAM transfer test" : targets[e->target]);
        text(32,329,label,190);
    }
    box(32,406,416,10,46,46,54);
    if (e->total) {
        int filled = (int)(416 * e->done / e->total);
        box(32,406,filled,10,e->error ? 225:168,e->error ? 95:130,e->error ? 95:246);
        char counter[96];
        snprintf(counter,sizeof counter,"%llu%%",100*e->done/e->total); text(32,433,counter,245);
        snprintf(counter,sizeof counter,"%.1f / %.1f MiB",e->done/1048576.0,e->total/1048576.0); text(32,486,counter,175);
        if(e->rate) snprintf(counter,sizeof counter,"%.2f MiB/s",e->rate/1048576.0);
        else snprintf(counter,sizeof counter,"Measuring transfer rate...");
        text(32,529,counter,175);
    } else text(32,433,"Waiting for progress...",175);
    box(32,629,416,1,48,48,54);
    char wifi[64]; snprintf(wifi,sizeof wifi,"WiFi: %s",networks[e->wifi]); text(32,655,wifi,205);
    if(e->error) {
        const char *explain[] = {"","Network interrupted","Verification failed","Storage check failed","Invalid transfer request"};
        text(32,708,explain[e->error],230);
    } else text(32,708,"Keep USB connected",140);
}
static int read_event(const char *path, struct event *event) {
    FILE *file=fopen(path,"r"); if(!file) return 0;
    char line[256]; int valid=fgets(line,sizeof line,file) && feof(file)==0;
    /* Require exactly one bounded line, including for atomically replaced files. */
    if(valid) valid=strchr(line,'\n') && fgetc(file)==EOF && parse(line,event);
    fclose(file); return valid;
}
static int ppm(const char *path) {
    FILE *file=fopen(path,"wb"); if(!file) return 0;
    fprintf(file,"P6\n%d %d\n255\n",W,H);
    int ok=fwrite(rgb,1,sizeof rgb,file)==sizeof rgb;
    return fclose(file)==0 && ok;
}
static int present(int fd, const struct fb_var_screeninfo *v, const struct fb_fix_screeninfo *f) {
    size_t size=(size_t)f->line_length*H;
    unsigned char *frame=calloc(1,size); if(!frame) return 0;
    for(int y=0;y<H;y++) for(int x=0;x<W;x++) {
        unsigned char *p=rgb+(y*W+x)*3;
        uint32_t color=((uint32_t)p[0]<<v->red.offset)|((uint32_t)p[1]<<v->green.offset)|((uint32_t)p[2]<<v->blue.offset);
        if(v->transp.length) color|=255u<<v->transp.offset;
        memcpy(frame+y*f->line_length+x*4,&color,4);
    }
    int ok=lseek(fd,0,SEEK_SET)==0;
    for(size_t at=0;ok && at<size;) {
        size_t count=size-at>4096?4096:size-at;
        ssize_t sent=write(fd,frame+at,count);
        if(sent<=0) ok=0; else at+=(size_t)sent;
    }
    free(frame); return ok;
}
int main(int argc,char **argv) {
    struct event current={0}, previous={0};
    if(argc==4 && !strcmp(argv[1],"--ppm")) {
        if(!read_event(argv[2],&current)) return 2;
        render(&current); return ppm(argv[3])?0:1;
    }
    if(argc!=1) return 2;
    int fd=open("/dev/fb0",O_WRONLY|O_CLOEXEC); if(fd<0) return 1;
    struct fb_var_screeninfo v; struct fb_fix_screeninfo f;
    if(ioctl(fd,FBIOGET_VSCREENINFO,&v)<0 || ioctl(fd,FBIOGET_FSCREENINFO,&f)<0 ||
       v.xres!=W || v.yres!=H || v.bits_per_pixel!=32 || f.line_length<W*4 || f.line_length>W*16 ||
       v.red.length!=8 || v.green.length!=8 || v.blue.length!=8 || v.red.offset>24 || v.green.offset>24 || v.blue.offset>24 ||
       (size_t)f.line_length*H>f.smem_len || v.red.offset==v.green.offset || v.red.offset==v.blue.offset || v.green.offset==v.blue.offset ||
       (v.transp.length && (v.transp.length!=8 || v.transp.offset>24))) { close(fd); return 1; }
    int first=1;
    for(;;) {
        struct event latest;
        if(read_event("/tmp/couch-installer-display.state",&latest)) current=latest;
        FILE *wifi=fopen("/tmp/couch-wifi.status","r");
        if(wifi) { char state[20]; if(fscanf(wifi,"%19s",state)==1) {int id=lookup(state,networks,5);if(id>=0) current.wifi=id;} fclose(wifi); }
        if(first || memcmp(&previous,&current,sizeof current)) { render(&current); if(!present(fd,&v,&f)) break; previous=current;first=0; }
        sleep(1);
    }
    close(fd); return 1;
}
