/* Disposable feasibility probe only: not a production authorization policy. */
#include <linux/bpf.h>

#define SEC(name) __attribute__((section(name), used))
#define FIELD(name, value) int (*name)[value]
#define TYPE(name, value) value *name

struct policy {
    __u32 tgid;
    __u32 port;
    __u64 deadline;
    __u64 checks;
};
struct {
    FIELD(type, BPF_MAP_TYPE_ARRAY);
    FIELD(max_entries, 1);
    TYPE(key, __u32);
    TYPE(value, struct policy);
} policy SEC(".maps");

/* CO-RE resolves these fields against the guest's kernel BTF. */
struct sock_common { __u16 skc_dport; __u16 skc_family; }
    __attribute__((preserve_access_index));
struct sock { struct sock_common __sk_common; }
    __attribute__((preserve_access_index));
struct socket { struct sock *sk; }
    __attribute__((preserve_access_index));

static void *(*lookup)(void *, const void *) = (void *)BPF_FUNC_map_lookup_elem;
static __u64 (*pid_tgid)(void) = (void *)BPF_FUNC_get_current_pid_tgid;
static __u64 (*now_ns)(void) = (void *)BPF_FUNC_ktime_get_ns;
static long (*read_kernel)(void *, __u32, const void *) =
    (void *)BPF_FUNC_probe_read_kernel;

SEC("lsm/socket_sendmsg")
int endpoint_send(__u64 *ctx)
{
    int previous = (int)ctx[3];
    if (previous)
        return previous;
    __u32 key = 0;
    struct policy *rule = lookup(&policy, &key);
    if (!rule || !rule->port)
        return 0;
    struct socket *socket = (void *)ctx[0];
    struct sock *sk = 0;
    __u16 port = 0, family = 0;
    if (read_kernel(&sk, sizeof(sk), &socket->sk) || !sk)
        return -1;
    if (read_kernel(&family, sizeof(family), &sk->__sk_common.skc_family))
        return -1;
    if (family != 2 && family != 10)
        return 0;
    if (read_kernel(&port, sizeof(port), &sk->__sk_common.skc_dport))
        return -1;
    if (__builtin_bswap16(port) != rule->port)
        return 0;
    __sync_fetch_and_add(&rule->checks, 1);
    if ((__u32)(pid_tgid() >> 32) != rule->tgid || now_ns() >= rule->deadline)
        return -1;
    return 0;
}

char LICENSE[] SEC("license") = "GPL";
