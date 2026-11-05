/* A deterministic offline PTY fixture for strict prompt delivery. It never
 * calls an upstream model or reads an existing agent profile. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

int main(int argc, char **argv) {
    const char *receipt=getenv("MATRIX_AGENT_RECEIPTS");
    if (receipt) {
        FILE *f=fopen(receipt,"a");
        if (!f) return 1;
        for (int i=1;i<argc;i++) fprintf(f,"%s%s",i==1?"":" ",argv[i]);
        fputc('\n',f);fclose(f);
    }
    int busy = argc > 1 && strcmp(argv[1], "busy") == 0;
    fputs("\033]0;Codex\007\033[2J\033[2;1H", stdout);
    fputs(busy ? "Generating answer (esc to cancel, 12s)" : "\xe2\x80\xba Ready for your next task", stdout);
    fflush(stdout);
    char line[4096];
    while (fgets(line, sizeof line, stdin)) {
        if (strncmp(line, ":fixture-ready", 14) == 0) {
            fputs("\033[2J\033[2;1H\xe2\x80\xba Ready for your next task", stdout);
            fflush(stdout);
            continue;
        }
        printf("fixture accepted: %s", line);
        fflush(stdout);
    }
    return 0;
}
