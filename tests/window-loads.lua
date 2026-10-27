-- Does Chimera's window open the project and run the core? It runs a few
-- frames, prints what it saw and leaves; run-frontend.sh reads the output.
for i = 1, 5 do emu.frameadvance() end
print("RUFFLE OK frames=" .. tostring(emu.framecount()))
client.exit()
