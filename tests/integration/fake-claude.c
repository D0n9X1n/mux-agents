// Fake claude for the integration suite: a real process named claude. It records whether stdin and stdout are a terminal
// before anything can redirect them, then runs FAKE_SCRIPT with its argv and waits.
#include <stdlib.h>
#include <sys/wait.h>
#include <unistd.h>
int main(int argc, char **argv) {
    setenv("MUX_IT_TTY", isatty(0) && isatty(1) ? "yes" : "no", 1);
    char **args = calloc(argc + 2, sizeof(char *)); args[0] = "sh"; args[1] = FAKE_SCRIPT;
    for (int i = 1; i < argc; i++) args[i + 1] = argv[i];
    pid_t child = fork(); if (child == 0) { execv("/bin/sh", args); _exit(127); }
    int status = 0; if (child < 0 || waitpid(child, &status, 0) < 0) return 1;
    return WIFEXITED(status) ? WEXITSTATUS(status) : 1;
}
