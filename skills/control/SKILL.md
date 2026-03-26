---
name: control
description: The ability to interact with the train set
tools: [bash, read_file, write_file]
---

# control

You have full control over the train set. You interact via the Points, Track and Sensors and read their status. This is done via REST calls to the Raspberry Pi called "pi". You need to use this power carefully. Use **bash** (for example `curl`) to call the HTTP API.

## Process

1. **Don't** use 100% power on curves. You risk a train falling off the track.
2. Expect to be told initially where the trains are.
3. You can simulate train movement by changing sensors yourself.
4. Never try to go through a point that is set against you unless you enter from the `thru` direction.

## Interface: REST calls

The REST API is reachable at `http://192.168.1.80:5000/api/<endpoint>`. There is no security because it is on a local LAN and not exposed to the internet. Calls are a mixture of GET and POST.

You can discover all calls from the OpenAPI document: http://192.168.1.80:5000/api/openapi.json

If you need different endpoints, they can be added on the Pi — note what you need and request a change.

### Track

Tracks are segments of track. They can be in one of four states:

1. `OFF` — there is no power to the segment. Any train on that segment is halted.
2. `FWD` — The train on that segment will move in a forward direction.
3. `BWD` — The train on that segment will move in a backward direction.
4. `HELD` — Status set only by the "pi". Some setting prevents safe movement of the train on this segment. Review the status to find out why.

### Points

These are sometimes called switches. They allow a train to be routed between different track segments. A point has three connections:

1. `entry` — A train can enter here and be routed to either the `thru` or `branch` connection.
2. `thru` — A train can enter if and only if the point is set to `thru`. It will exit on the `entry` track segment.
3. `branch` — A train can enter if and only if the point is set to `branch`. It will exit on the `entry` track segment.

### Sensors

These detect the presence of a train at a specific point on a track segment. They remain set until the hardware in the Pi resets the sensor. That typically happens when the train is detected on a following sensor.

**Note:** You can exercise these sensors manually when testing by sending a command to set or clear a sensor. That lets you test controls without trains on the circuit — you can simulate running trains.
