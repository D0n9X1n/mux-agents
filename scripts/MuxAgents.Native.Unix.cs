// Linux and macOS natives for MuxAgents.Unix.ps1. DllImport binds only when called, so the type compiles on every OS; pure parsers are public.
using System; using System.Runtime.InteropServices; using System.Text;
public static class MuxAgentUnixNative {
    [DllImport("libc.so.6", SetLastError = true)] static extern long syscall(long number, long a, long b, long c, long d);
    [DllImport("libc.so.6")] static extern int poll(int[] fds, ulong count, int timeout);
    [DllImport("libc.so.6", EntryPoint = "close")] static extern int LinuxClose(int fd);
    [DllImport("/usr/lib/libSystem.dylib")] static extern int sysctl(int[] name, uint count, byte[] buffer, ref UIntPtr size, IntPtr newValue, UIntPtr newSize);
    [DllImport("/usr/lib/libSystem.dylib")] static extern int sandbox_check(int pid, IntPtr operation, int type);
    [DllImport("/usr/lib/libSystem.dylib")] static extern int proc_pidinfo(int pid, int flavor, ulong arg, byte[] buffer, int size);
    // pidfd_open 434 and pidfd_send_signal 424 (x86_64 and arm64 share the asm-generic numbers since 5.1). A pidfd polls readable once its process exited.
    public static int PidfdOpen(int pid) { long fd = syscall(434, pid, 0, 0, 0); return fd < 0 ? -1 : (int)fd; }
    public static bool PidfdKill(int fd) { return syscall(424, fd, 9, 0, 0) == 0; }
    // struct pollfd { int fd; short events; short revents; }: events POLLIN 1 in the low half of item[1], revents in the high half.
    public static bool PidfdExited(int fd, int milliseconds) { int[] item = { fd, 1 }; int count = poll(item, 1, milliseconds); return PollShowsExit(count, (item[1] >> 16) & 0xFFFF); }
    // Exit is one ready entry with POLLIN 1 set and none of POLLERR 8 or POLLNVAL 32 (an fd that is not open); POLLHUP 16 may join POLLIN.
    public static bool PollShowsExit(int count, int revents) { return count == 1 && (revents & 1) != 0 && (revents & (8 | 32)) == 0; }
    // realpath(3) into our own PATH_MAX buffer (4096 on Linux, 1024 on macOS): every link in every component resolved; null on failure.
    [DllImport("libc.so.6", EntryPoint = "realpath")] static extern IntPtr LinuxRealPath([MarshalAs(UnmanagedType.LPUTF8Str)] string path, byte[] resolved);
    [DllImport("/usr/lib/libSystem.dylib", EntryPoint = "realpath")] static extern IntPtr MacRealPath([MarshalAs(UnmanagedType.LPUTF8Str)] string path, byte[] resolved);
    public static string RealPathLinux(string path) { byte[] buffer = new byte[4097]; return ReadRealPath(LinuxRealPath(path, buffer), buffer); }
    public static string RealPathMac(string path) { byte[] buffer = new byte[4097]; return ReadRealPath(MacRealPath(path, buffer), buffer); }
    static string ReadRealPath(IntPtr result, byte[] buffer) {
        if (result == IntPtr.Zero) return null;
        int end = Array.IndexOf(buffer, (byte)0); return end <= 0 ? null : Encoding.UTF8.GetString(buffer, 0, end); }
    public static void Close(int fd) { LinuxClose(fd); }
    // kill(pid, 0) sends no signal; it only checks the pid. 0 when the process exists, else errno: ESRCH 3 is "no such process"
    // (3 on Linux and macOS); EPERM 1 means it exists under another user. A pid below 1 names a group, so it is EINVAL 22 here.
    [DllImport("libc.so.6", EntryPoint = "kill", SetLastError = true)] static extern int LinuxSignal(int pid, int signal);
    [DllImport("/usr/lib/libSystem.dylib", EntryPoint = "kill", SetLastError = true)] static extern int MacSignal(int pid, int signal);
    public static int ProbeLinux(int pid) { if (pid < 1) return 22; return LinuxSignal(pid, 0) == 0 ? 0 : Marshal.GetLastPInvokeError(); }
    public static int ProbeMac(int pid) { if (pid < 1) return 22; return MacSignal(pid, 0) == 0 ? 0 : Marshal.GetLastPInvokeError(); }
    public static string[] ProcArgs(int pid) {  // CTL_KERN 1, KERN_PROCARGS2 49
        byte[] buffer = new byte[1 << 20]; UIntPtr size = (UIntPtr)buffer.Length;
        return sysctl(new int[] { 1, 49, pid }, 3, buffer, ref size, IntPtr.Zero, UIntPtr.Zero) != 0 ? null : ParseProcArgs(buffer, (int)size); }
    // KERN_PROCARGS2: int argc, exec path, NUL padding up to the next offset 4 + 8k, then argc NUL-terminated strings. The padding
    // must be all NUL and argv[0] non-empty (an empty argv[0] cannot be told from padding). Any other layout is unreadable.
    public static string[] ParseProcArgs(byte[] buffer, int length) {
        if (length < 4) return null;
        int argc = BitConverter.ToInt32(buffer, 0), pathEnd = Array.IndexOf(buffer, (byte)0, 4, length - 4);
        if (argc < 1 || pathEnd < 0) return null;
        int at = 4 + ((pathEnd + 4) & ~7);
        for (int pad = pathEnd; pad < Math.Min(at, length); pad++) if (buffer[pad] != 0) return null;
        if (at >= length || buffer[at] == 0) return null;
        string[] argv = new string[argc];
        for (int index = 0; index < argc; index++) {
            int end = Array.IndexOf(buffer, (byte)0, at, Math.Max(0, length - at));
            if (end < 0) return null;
            argv[index] = Encoding.UTF8.GetString(buffer, at, end - at); at = end + 1; }
        return argv; }
    public static int SandboxCheck() { return sandbox_check(Environment.ProcessId, IntPtr.Zero, 0); }
    // PROC_PIDVNODEPATHINFO 9: proc_vnodepathinfo is 2352 bytes; pvi_cdir.vip_path starts at byte 152, after vnode_info.
    public static string Cwd(int pid) { byte[] info = new byte[2352]; return ParseVnodeCwd(info, proc_pidinfo(pid, 9, 0, info, info.Length)); }
    public static string ParseVnodeCwd(byte[] info, int returned) { if (returned != 2352) return null; int end = Array.IndexOf(info, (byte)0, 152, 1024); return end <= 152 ? null : Encoding.UTF8.GetString(info, 152, end - 152); }
    public static long EpochMicros(DateTime start) { return (start.ToUniversalTime().Ticks - DateTime.UnixEpoch.Ticks) / 10; } }
