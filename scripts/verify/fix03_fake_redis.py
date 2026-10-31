#!/usr/bin/env python3
"""FIX-03 用「可控假 Redis」：内存键值 + 可切换 alive/dead。

为什么不用真 Redis：本机（172.18.10.99）没有 redis-server / docker，而验收要求
「先有缓存把数据读进缓存 → 缓存不可用 → 业务照常成功 → 缓存恢复后不得读到旧数据」。
假 Redis 能模拟「进程还在、连接被拒（= Redis 挂了但内存里的旧值没丢）」，这正是
BUG-SYS-03 里「旧缓存打不掉」最难查的那个形态。

控制口（默认数据口 +1）：一行文本
    die    → 之后所有连接读一行后立刻关闭（客户端表现为 IO error，等价 Redis 不可用）
    live   → 恢复正常
    dump   → 返回当前键数量（stderr 打印，便于断言缓存里是否真留着旧值）
    flush  → 清空内存键值
"""
import socket
import socketserver
import sys
import threading
import time

STORE = {}
EXPIRY = {}
LOCK = threading.Lock()
STATE = {"dead": False, "del_fail": False}


def now():
    return time.time()


def expired(key):
    exp = EXPIRY.get(key)
    return exp is not None and exp < now()


class RespHandler(socketserver.BaseRequestHandler):
    def read_command(self):
        buf = b""
        while b"\r\n" not in buf:
            chunk = self.request.recv(1)
            if not chunk:
                return None
            buf += chunk
        line = buf.decode("utf-8", "replace").strip()
        if not line:
            return None
        if not line.startswith("*"):
            return line.split()
        try:
            n = int(line[1:])
        except ValueError:
            return None
        args = []
        for _ in range(n):
            head = b""
            while b"\r\n" not in head:
                chunk = self.request.recv(1)
                if not chunk:
                    return None
                head += chunk
            try:
                ln = int(head.decode().strip()[1:])
            except ValueError:
                return None
            payload = b""
            while len(payload) < ln + 2:
                chunk = self.request.recv(ln + 2 - len(payload))
                if not chunk:
                    return None
                payload += chunk
            args.append(payload[:ln].decode("utf-8", "replace"))
        return args

    def handle(self):
        while True:
            if STATE["dead"]:
                # 读完一条命令就断开，不回复 —— 客户端得到 IO error / connection reset
                try:
                    self.read_command()
                except Exception:
                    pass
                return
            try:
                args = self.read_command()
            except (ConnectionResetError, BrokenPipeError):
                return
            if args is None:
                return
            if not args:
                continue
            try:
                self.request.sendall(dispatch(args))
            except (ConnectionResetError, BrokenPipeError):
                return


def bulk(value):
    if value is None:
        return b"$-1\r\n"
    raw = str(value).encode()
    return b"$" + str(len(raw)).encode() + b"\r\n" + raw + b"\r\n"


def integer(n):
    return b":" + str(n).encode() + b"\r\n"


def simple(text):
    return b"+" + text.encode() + b"\r\n"


def array(items):
    out = b"*" + str(len(items)).encode() + b"\r\n"
    for item in items:
        out += bulk(item)
    return out


def get_key(key):
    if expired(key):
        STORE.pop(key, None)
        EXPIRY.pop(key, None)
        return None
    return STORE.get(key)


def dispatch(args):
    cmd = args[0].upper()
    with LOCK:
        if cmd == "PING":
            return simple("PONG")
        if cmd == "SET":
            STORE[args[1]] = args[2]
            EXPIRY.pop(args[1], None)
            for i, a in enumerate(args):
                if a.upper() == "EX" and i + 1 < len(args):
                    EXPIRY[args[1]] = now() + float(args[i + 1])
                if a.upper() == "PX" and i + 1 < len(args):
                    EXPIRY[args[1]] = now() + float(args[i + 1]) / 1000.0
            return simple("OK")
        if cmd == "SETEX":
            STORE[args[1]] = args[3]
            EXPIRY[args[1]] = now() + float(args[2])
            return simple("OK")
        if cmd == "PSETEX":
            STORE[args[1]] = args[3]
            EXPIRY[args[1]] = now() + float(args[2]) / 1000.0
            return simple("OK")
        if cmd == "SETNX":
            if get_key(args[1]) is None:
                STORE[args[1]] = args[2]
                return integer(1)
            return integer(0)
        if cmd == "MSET":
            for i in range(1, len(args) - 1, 2):
                STORE[args[i]] = args[i + 1]
            return simple("OK")
        if cmd == "MGET":
            return array([get_key(k) for k in args[1:]])
        if cmd == "GET":
            return bulk(get_key(args[1]))
        if cmd == "DEL" or cmd == "UNLINK":
            if STATE["del_fail"]:
                # 只让「失效」失败：模拟「Redis 还活着，但这一次 DEL 超时/报错」——
                # 正是 BUG-SYS-03 最难复现的形态（业务写库成功、缓存打不掉）
                return b"-ERR simulated DEL failure (fix03)\r\n"
            n = 0
            for k in args[1:]:
                if get_key(k) is not None:
                    n += 1
                STORE.pop(k, None)
                EXPIRY.pop(k, None)
            return integer(n)
        if cmd == "EXISTS":
            return integer(sum(1 for k in args[1:] if get_key(k) is not None))
        if cmd == "INCR":
            cur = int(get_key(args[1]) or 0) + 1
            STORE[args[1]] = str(cur)
            return integer(cur)
        if cmd == "INCRBY":
            cur = int(get_key(args[1]) or 0) + int(args[2])
            STORE[args[1]] = str(cur)
            return integer(cur)
        if cmd == "EXPIRE":
            EXPIRY[args[1]] = now() + float(args[2])
            return integer(1)
        if cmd == "PEXPIRE":
            EXPIRY[args[1]] = now() + float(args[2]) / 1000.0
            return integer(1)
        if cmd == "TTL":
            exp = EXPIRY.get(args[1])
            return integer(-1 if exp is None else int(exp - now()))
        if cmd == "PTTL":
            exp = EXPIRY.get(args[1])
            return integer(-1 if exp is None else int((exp - now()) * 1000))
        if cmd in ("KEYS", "SCAN"):
            keys = [k for k in STORE if get_key(k) is not None]
            if cmd == "SCAN":
                return b"*2\r\n" + bulk("0") + array(keys)
            return array(keys)
        if cmd == "FLUSHDB" or cmd == "FLUSHALL":
            STORE.clear()
            EXPIRY.clear()
            return simple("OK")
        if cmd == "TYPE":
            return simple("string" if get_key(args[1]) is not None else "none")
        if cmd == "HGET":
            return bulk(None)
        if cmd == "HSET" or cmd == "HMSET":
            return integer(1)
        if cmd == "HGETALL":
            return array([])
        if cmd == "INFO":
            return bulk("redis_version:9.9.9-fake\r\n")
        if cmd == "CLIENT":
            return simple("OK")
        if cmd == "SELECT":
            return simple("OK")
        if cmd == "AUTH":
            return simple("OK")
        # 兜底：未知命令一律 OK，避免假服务器把业务路径卡住
        sys.stderr.write("[fake-redis] unknown cmd: %s\n" % " ".join(args[:2]))
        sys.stderr.flush()
        return simple("OK")


class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


class ControlHandler(socketserver.StreamRequestHandler):
    def handle(self):
        line = self.rfile.readline().decode().strip().lower()
        if line == "die":
            STATE["dead"] = True
            self.wfile.write(b"dead\n")
        elif line == "live":
            STATE["dead"] = False
            self.wfile.write(b"live\n")
        elif line == "flush":
            with LOCK:
                STORE.clear()
                EXPIRY.clear()
            self.wfile.write(b"flushed\n")
        elif line == "del_fail":
            STATE["del_fail"] = True
            self.wfile.write(b"del_fail\n")
        elif line == "del_ok":
            STATE["del_fail"] = False
            self.wfile.write(b"del_ok\n")
        elif line == "dump":
            with LOCK:
                keys = sorted(STORE.keys())
            self.wfile.write(("keys=%d %s\n" % (len(keys), keys)).encode())
        else:
            self.wfile.write(b"usage: die|live|flush|dump|del_fail|del_ok\n")


def main():
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 6399
    data = Server(("127.0.0.1", port), RespHandler)
    ctl = Server(("127.0.0.1", port + 1), ControlHandler)
    threading.Thread(target=ctl.serve_forever, daemon=True).start()
    sys.stderr.write("[fake-redis] listening data=%d control=%d\n" % (port, port + 1))
    sys.stderr.flush()
    data.serve_forever()


if __name__ == "__main__":
    main()
