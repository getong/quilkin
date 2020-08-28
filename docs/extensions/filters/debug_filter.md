# Debug

The Debug filter logs all incoming and outgoing packets to standard output.

This filter is useful in debugging deployments where the packets strictly contain valid `UTF-8` encoded strings. A generic error message is instead logged if conversion from bytes to `UTF-8` fails.

#### Filter name
```text
quilkin.extensions.filters.debug_filter.v1alpha1.DebugFilter
```

### Configuration Examples
```rust
# let yaml = "
local:
  port: 7000
filters:
  - name: quilkin.extensions.filters.debug_filter.v1alpha1.DebugFilter
    config:
      id: debug-1
client:
  addresses:
    - 127.0.0.1:7001
  connection_id: MXg3aWp5Ng==
# ";
# let config = quilkin::config::Config::from_reader(yaml.as_bytes()).unwrap();
# assert_eq!(config.validate().unwrap(), ());
# assert_eq!(config.filters.len(), 1);
# // TODO: make it possible to easily validate filter's config from here.
```

### Configuration Options

```yaml
properties:
  id:
    type: string
    description: |
      An identifier that will be included with each log message.
```


### Metrics

This filter currently exports no metrics.