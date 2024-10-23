# cosmic-ext-alternative-startup

This provides an alternative entry point for [`cosmic-session`](https://github.com/pop-os/cosmic-session)s
compositor ipc interface usually provided by [`cosmic-comp`](https://github.com/pop-os/cosmic-comp).

## Why does this IPC API exist?

1. `cosmic-session` needs to know when the compositor has setup the necessary sockets to race-free
launch dependent shell-components. This job could be done by other service managers, such as systemd,
but is currently provided in an agnostic way by `cosmic-session`.
2. `cosmic-session` and `cosmic-comp` work together to give special privileged connections to
it's shell clients. While certainly not flawless, it enables the compositor to only trust
the process that has been starting itself with certain functionality.

## What does `cosmic-ext-alternative-startup` do?

For sessions without `cosmic-comp` is poses a problem. They usually have their own ways of
starting apps after startup or notify a service manager like systemd.

`cosmic-ext-alternative-startup` can be used by these to fire up a small daemon that will
notify `cosmic-session` of successful startup on execution.

It will also handle requests for "privileged" wayland connections by simply forwarding these
connections to the normal wayland socket, for other compositors don't have a matching api
and just expose privileged protocols to all non-sandboxed clients.

## So how do I use it?

Depends on the compositor you want to use with COSMIC!
In general please take a look at [`cosmic-ext-extra-sessions`](https://github.com/drakulix/cosmic-ext-extra-sessions`) instead.

But e.g. in sway you could add `exec cosmic-ext-alternative-startup` to the end of your configuration file
to be able to launch a sway cosmic session using `cosmic-session sway`.
