#!/bin/sh
# Fake Proton Rocket League for detector tests: the game path sits on the command line.
exec -a x sh -c "sleep ${1:-45}; true" "/mnt/data/SteamLibrary/steamapps/common/rocketleague/RocketLeague.exe"
