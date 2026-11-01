# Fake Minecraft for detector tests: process named "java", "minecraft" in argv.
import ctypes, time
ctypes.CDLL(None).prctl(15, b"java", 0, 0, 0)
time.sleep(40)
