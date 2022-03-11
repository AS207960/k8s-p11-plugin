# Kubernetes device plugin for PKCS#11 devices

A Kubernetes device plugin to provide access to HSMs within pods.
`p11-kit` is used to forward commands over a unix socket through the container boundary.

## Example config

```yaml
p11_kit_binary: /usr/bin/p11-kit
hsm_types:
  - name: test
    tokens:
      - "pkcs11:model=YubiKey%20YK5;manufacturer=Yubico%20%28www.yubico.com%29;serial=17040761;token=YubiKey%20PIV%20%2317040761"
    max_clients: 10
    provider: /usr/lib/x86_64-linux-gnu/libykcs11.so
```

### `p11_kit_binary`

Location of the `p11-kit` binary for the server side of the socket.

### `hsm_types`

Different HSMs (or sets of HSMs) that will be exposed as a device type to k8s.

### `hsm_types.name`

Name to give to the device in k8s, will be of the form `hsm.as207960.net/{name}`.

### `hsm_types.tokens`

A list of token URLs to group into this HSM set. These must all be from the same provider.
A list of possible token URLs can be obtained with `p11tool --list-tokens`.

### `hsm_types.provider` (optional)

If you'd like to use a non-standard provider for PKCS#11 (such as `libykcs11.so`) provide
the full path to the `.so` file here.

### `hsm_types.max_clients`

In theory an unlimited number of pods can have access to a HSM set, but k8s requires we define a finite
number of resources, so this value specifies how many pods can access this HSM set at once.

## Use inside pod

Regardless of how many of each HSM set is requested (you should only request 1 anyway)
`/dev/pkcs11-{name}` will be mounted inside the pod, giving access to the `p11-kit` server for
each set. The environment variable `P11_KIT_SERVER_ADDRESS` will also be set to the first
requested set, to provide a default if only one set is requested. If more than one set is 
requested you'll have to manage this environment variable yourself.

The PKCS#11 provider `p11-kit-client.so` from `p11-kit` can then be used to access the HSM,
with the environment variable configured correctly.