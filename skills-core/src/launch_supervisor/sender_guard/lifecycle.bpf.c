/* Sender guard ownership/loss hooks, embedded in the measured launcher.
 * Loaded and pinned by the root per-Session supervisor, never the broker. */
#define LIFECYCLE_PROOF
#include "binding.bpf.c"

struct {
    FIELD(type, BPF_MAP_TYPE_TASK_STORAGE);
    FIELD(map_flags, BPF_F_NO_PREALLOC);
    TYPE(key, int);
    TYPE(value, __u32);
} owners SEC(".maps");
struct {
    FIELD(type, BPF_MAP_TYPE_HASH);
    FIELD(max_entries, 16);
    TYPE(key, __u32);
    TYPE(value, __u32);
} lost SEC(".maps");

/* Supervisor-created outbound sockets, registered before descriptor handoff.
 * The broker has no writable map handle. Socket lifetime, not a port or fd
 * number, owns each binding; unrelated sockets do not consult our loss latch. */
struct upstream_binding {
    struct binding authority;
    __u32 endpoint_port, port;
};
struct {
    FIELD(type, BPF_MAP_TYPE_SK_STORAGE);
    FIELD(map_flags, BPF_F_NO_PREALLOC);
    TYPE(key, int);
    TYPE(value, struct upstream_binding);
} upstreams SEC(".maps");

static __attribute__((always_inline)) int owner_lost(void)
{
    __u32 key = 0;
    __u32 *revoked = lookup(&lost, &key);
    return !revoked || *revoked;
}

static __attribute__((always_inline)) int upstream_send(
    struct sock *sk, struct upstream_binding *bound)
{
    struct binding *rule = &bound->authority;
    struct binding *active = lookup(&policy, &bound->endpoint_port);
    struct grant *sender = task_get(&tasks, current()->group_leader, 0, 0);
    if (owner_lost() || !active || !sender || sender->revoked ||
        sender->launch != rule->launch || sender->session != rule->session ||
        sender->run != rule->run || !rule->revision ||
        active->session != rule->session || active->run != rule->run ||
        active->revision != rule->revision || active->listener != rule->listener ||
        now_ns() >= rule->deadline || now_ns() >= active->deadline ||
        !lookup(&listeners, &rule->listener))
        return -1;
    if (sk->__sk_common.skc_net.net->ns.inum != rule->netns ||
        sk->__sk_common.skc_family != rule->family ||
        __builtin_bswap16(sk->__sk_common.skc_dport) != bound->port)
        return -1;
    if (rule->family == 2) {
        if (sk->__sk_common.skc_daddr != rule->address[0])
            return -1;
    } else if (rule->family == 10) {
        for (int i = 0; i < 4; i++)
            if (sk->__sk_common.skc_v6_daddr.in6_u.u6_addr32[i] != rule->address[i])
                return -1;
    } else {
        return -1;
    }
    return 0;
}

/* Kernel task lifetime, not a userspace heartbeat or a reusable numeric PID.
 * A leader exit conservatively revokes even if another owner thread lives.
 * This latch is never cleared; replacing an owner requires a fresh launch.
 */
static __attribute__((always_inline)) int revoke_owner(void)
{
    struct task_struct *task = current();
    if (task != task->group_leader)
        return 0;
    __u32 *key = task_get(&owners, task, 0, 0);
    if (key) {
        __u32 *revoked = lookup(&lost, key);
        if (revoked)
            *(volatile __u32 *)revoked = 1;
    }
    return 0;
}

SEC("raw_tp/sched_process_exit")
int owner_exit(__u64 *ctx)
{
    return revoke_owner();
}

SEC("lsm/bprm_committed_creds")
int owner_exec(__u64 *ctx)
{
    return revoke_owner();
}

SEC("lsm/socket_sendmsg")
int endpoint_send(__u64 *ctx)
{
    int previous = (int)ctx[3];
    if (previous)
        return previous;
    struct socket *socket = (void *)ctx[0];
    struct sock *sk = socket->sk;
    if (!sk)
        return -1;
    struct upstream_binding *bound = socket_get(&upstreams, sk, 0, 0);
    if (bound)
        return upstream_send(sk, bound);
    int result = binding_send(ctx);
    if (result)
        return result;
    __u32 family = sk->__sk_common.skc_family;
    if (family != 2 && family != 10)
        return 0;
    __u32 port = __builtin_bswap16(sk->__sk_common.skc_dport);
    __u32 *namespace = lookup(&ports, &port);
    if (!namespace || *namespace != sk->__sk_common.skc_net.net->ns.inum)
        return 0;
    return owner_lost() ? -1 : 0;
}
