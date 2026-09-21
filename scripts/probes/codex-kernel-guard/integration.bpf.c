/* Diagnostic exact-binding guard for the full ACP integration proof. */
#define LIFECYCLE_PROOF
#include "binding.bpf.c"

struct decision {
    __u64 pid_tgid;
    __u32 reason, family, port, socket_netns, task_netns;
};
struct {
    FIELD(type, BPF_MAP_TYPE_ARRAY);
    FIELD(max_entries, 1);
    TYPE(key, __u32);
    TYPE(value, struct decision);
} decision SEC(".maps");

static __u64 (*pid_tgid)(void) = (void *)BPF_FUNC_get_current_pid_tgid;

static __attribute__((always_inline)) int record(
    __u32 reason, __u32 family, __u32 port, __u32 socket_netns, __u32 task_netns)
{
    __u32 key = 0;
    struct decision *value = lookup(&decision, &key);
    if (value) {
        value->pid_tgid = pid_tgid();
        value->reason = reason;
        value->family = family;
        value->port = port;
        value->socket_netns = socket_netns;
        value->task_netns = task_netns;
    }
    return reason ? -1 : 0;
}

SEC("lsm/socket_sendmsg")
int endpoint_send(__u64 *ctx)
{
    if (!ctx[0])
        return binding_send(ctx);
    int previous = (int)ctx[3];
    if (previous)
        return previous;
    struct socket *socket = (void *)ctx[0];
    struct sock *sk = socket->sk;
    if (!sk)
        return record(1, 0, 0, 0, 0);
    __u32 family = sk->__sk_common.skc_family;
    if (family != 2 && family != 10)
        return 0;
    __u32 port = __builtin_bswap16(sk->__sk_common.skc_dport);
    if (!lookup(&ports, &port))
        return 0;
    struct binding *rule = lookup(&policy, &port);
    struct task_struct *task = current();
    struct grant *grant = task_get(&tasks, task->group_leader, 0, 0);
    __u32 socket_netns = sk->__sk_common.skc_net.net->ns.inum;
    __u32 task_netns = task->nsproxy->net_ns->ns.inum;
    if (!rule)
        return record(2, family, port, socket_netns, task_netns);
    if (!grant)
        return record(3, family, port, socket_netns, task_netns);
    if (grant->revoked)
        return record(4, family, port, socket_netns, task_netns);
    if (grant->launch != rule->launch || grant->session != rule->session || grant->run != rule->run)
        return record(5, family, port, socket_netns, task_netns);
    if (!rule->revision || now_ns() >= rule->deadline)
        return record(6, family, port, socket_netns, task_netns);
    if (!lookup(&listeners, &rule->listener))
        return record(7, family, port, socket_netns, task_netns);
    if (family != rule->family)
        return record(8, family, port, socket_netns, task_netns);
    if (socket_netns != rule->netns)
        return record(9, family, port, socket_netns, task_netns);
    if (task_netns != rule->netns)
        return record(10, family, port, socket_netns, task_netns);
    if (family == 2) {
        if (sk->__sk_common.skc_daddr != rule->address[0])
            return record(11, family, port, socket_netns, task_netns);
    } else {
        for (int i = 0; i < 4; i++)
            if (sk->__sk_common.skc_v6_daddr.in6_u.u6_addr32[i] != rule->address[i])
                return record(11, family, port, socket_netns, task_netns);
    }
    struct stamp initial = {rule->launch, rule->revision, rule->listener};
    struct stamp *stamp = socket_get(&connections, sk, &initial, BPF_LOCAL_STORAGE_GET_F_CREATE);
    if (!stamp || stamp->launch != rule->launch || stamp->revision != rule->revision ||
        stamp->listener != rule->listener)
        return record(12, family, port, socket_netns, task_netns);
    return record(0, family, port, socket_netns, task_netns);
}
