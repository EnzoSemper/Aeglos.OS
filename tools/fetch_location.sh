#!/bin/bash
LOC_DATA=$(curl -s --connect-timeout 2 http://ip-api.com/json/)
if [ $? -eq 0 ] && [ -n "$LOC_DATA" ]; then
    LAT=$(echo "$LOC_DATA" | grep -o '"lat":[^,]*' | cut -d: -f2)
    LON=$(echo "$LOC_DATA" | grep -o '"lon":[^,]*' | cut -d: -f2)
    NS="N"
    EW="E"
    if (( $(echo "$LAT < 0" | bc -l) )); then NS="S"; LAT=${LAT#-}; fi
    if (( $(echo "$LON < 0" | bc -l) )); then EW="W"; LON=${LON#-}; fi
    COORD="${LAT}${NS} ${LON}${EW}"
else
    COORD="UNKNOWN"
fi

cat << RUST_EOF > userspace/ash/src/location.rs
pub const GPS_COORD: &[u8] = b"${COORD}";
pub const GPS_CHIP: bool = false; // No hardware GPS detected
RUST_EOF
