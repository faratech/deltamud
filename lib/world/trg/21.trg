#2188
Couplet Trigger for Trig 2189 - Spec Room Assignment~
2 c 100
exit~
wechoaround %actor% %actor.name% leaves the hut.
wsend %actor% You leave the hut.
wteleport %actor% LGH1
wechoaround %actor% %actor.name% emerges from the hut.
wforce %actor% look
~
#2189
Object Trigger for Item 2199~
1 c 4
enter~
if %arg% == hut
   oechoaround %actor% %actor.name% enters the hut.
   osend %actor% You enter the hut.
   oteleport %actor% 2191
   oechoaround %actor% %actor.name% enters the hut.
   oforce %actor% look
elseif %arg% != hut
   osend %actor% Huh?!?
~
#2190
Room Trigg for Rm 2169~
2 g 100
~
set gib %random.10%
eval MAN (%random.2% + 2100)
eval GOB (%random.3% + 2175)
if %gib% == 1 || 2 || 3
   set SE %MAN%
elseif %gib% == 4 || 5 || 6
   set SE 2129
elseif %gib% == 7 || 8
   set SE 2100
elseif %gib% == 9
   set SE 2134
else
   set SE %GOB%
end
wecho The trees seem to shift before your gaze.
wdoor 2169 south room %SE%
~
#2191
Room Trigg for Rm 2168~
2 g 100
~
set gib %random.10%
eval MAN (%random.2% + 2100)
eval GOB (%random.3% + 2175)
if %gib% == 1 || 2 || 3
   set WE %MAN%
elseif %gib% == 4 || 5 || 6
   set WE 2129
elseif %gib% == 7 || 8
   set WE 2100
elseif %gib% == 9
   set WE 2134
else
   set WE %GOB%
end
wecho The trees seem to shift before your gaze.
wdoor 2168 west room %WE%
~
#2192
Room Trigg for Rm 2167~
2 g 100
~
set gib %random.10%
eval MAN (%random.2% + 2100)
eval GOB (%random.3% + 2175)
if %gib% == 1 || 2 || 3
   set NE %MAN%
elseif %gib% == 4 || 5 || 6
   set NE 2129
elseif %gib% == 7 || 8
   set NE 2100
elseif %gib% == 9
   set NE 2134
else
   set NE %GOB%
end
wecho The trees seem to shift before your gaze.
wdoor 2167 north room %NE%
~
#2193
Room Trigg for Rm 2166~
2 g 100
~
set gib %random.10%
eval MAN (%random.2% + 2100)
eval GOB (%random.3% + 2175)
if %gib% == 1 || 2 || 3
   set EE %MAN%
elseif %gib% == 4 || 5 || 6
   set EE 2129
elseif %gib% == 7 || 8
   set EE 2100
elseif %gib% == 9
   set EE 2134
else
   set EE %GOB%
end
wecho The trees seem to shift before your gaze.
wdoor 2166 east room %EE%
~
#2194
Room Trigg for Rm 2103~
2 g 100
~
if %random.2% == 2
   set NE 2139
else
   set NE 2142
end
eval EE (%random.2% + 2130)
eval SE (%random.2% + 2139)
eval WE (%random.2% + 2140)
wecho The trees seem to shift before your gaze.
if (%direction% == north )
   wdoor 2103 north room 2114
   wdoor 2103 east room %EE%
   wdoor 2103 south room %SE%
   wdoor 2103 west room %WE%
elseif (%direction% == east )
   wdoor 2103 north room %NE%
   wdoor 2103 east room 2112
   wdoor 2103 south room %SE%
   wdoor 2103 west room %WE%
elseif (%direction% == south )
   wdoor 2103 north room %NE%
   wdoor 2103 east room %EE%
   wdoor 2103 south room 2114
   wdoor 2103 west room %WE%
elseif (%direction% == west )
   wdoor 2103 north room %NE%
   wdoor 2103 east room %EE%
   wdoor 2103 south room %SE%
   wdoor 2103 west room 2114
end
~
#2195
Room Trigg for Rm 2102~
2 g 100
~
if %random.2% == 2
   set NE 2139
else
   set NE 2142
end
eval EE (%random.2% + 2130)
eval SE (%random.2% + 2139)
eval WE (%random.2% + 2140)
wecho The trees seem to shift before your gaze.
if (%direction% == north )
   wdoor 2102 north room 2101
   wdoor 2102 east room %EE%
   wdoor 2102 south room %SE%
   wdoor 2102 west room %WE%
elseif (%direction% == east )
   wdoor 2102 north room %NE%
   wdoor 2102 east room 2101
   wdoor 2102 south room %SE%
   wdoor 2102 west room %WE%
elseif (%direction% == south )
   wdoor 2102 north room %NE%
   wdoor 2102 east room %EE%
   wdoor 2102 south room 2112
   wdoor 2102 west room %WE%
elseif (%direction% == west )
   wdoor 2102 north room %NE%
   wdoor 2102 east room %EE%
   wdoor 2102 south room %SE%
   wdoor 2102 west room 2101
end
~
#2196
Room Trigg for Rm 2175~
2 g 100
~
if %random.2% == 2
   set NE 2139
else
   set NE 2142
end
eval EE (%random.2% + 2130)
eval SE (%random.2% + 2139)
eval WE (%random.2% + 2140)
wecho The trees seem to shift before your gaze.
if (%direction% == north )
   wdoor 2175 north room 2180
   wdoor 2175 east room %EE%
   wdoor 2175 south room %SE%
   wdoor 2175 west room %WE%
elseif (%direction% == east )
   wdoor 2175 north room %NE%
   wdoor 2175 east room 2184
   wdoor 2175 south room %SE%
   wdoor 2175 west room %WE%
elseif (%direction% == south )
   wdoor 2175 north room %NE%
   wdoor 2175 east room %EE%
   wdoor 2175 south room 2190
   wdoor 2175 west room %WE%
elseif (%direction% == west )
   wdoor 2175 north room %NE%
   wdoor 2175 east room %EE%
   wdoor 2175 south room %SE%
   wdoor 2175 west room 2185
end
~
#2197
Room Trigg for Rm 2176~
2 g 100
~
if %random.2% == 2
   set NE 2139
else
   set NE 2142
end
eval EE (%random.2% + 2130)
eval SE (%random.2% + 2139)
eval WE (%random.2% + 2140)
wecho The trees seem to shift before your gaze.
if (%direction% == north )
   wdoor 2176 north room 2181
   wdoor 2176 east room %EE%
   wdoor 2176 south room %SE%
   wdoor 2176 west room %WE%
elseif (%direction% == east )
   wdoor 2176 north room %NE%
   wdoor 2176 east room 2187
   wdoor 2176 south room %SE%
   wdoor 2176 west room %WE%
elseif (%direction% == south )
   wdoor 2176 north room %NE%
   wdoor 2176 east room %EE%
   wdoor 2176 south room 2189
   wdoor 2176 west room %WE%
elseif (%direction% == west )
   wdoor 2176 north room %NE%
   wdoor 2176 east room %EE%
   wdoor 2176 south room %SE%
   wdoor 2176 west room 2182
end
~
#2198
Room Trigg for Rm 2177~
2 g 100
~
eval EE (%random.2% + 2130)
eval WE (%random.2% + 2140)
wecho The trees seem to shift before your gaze.
elseif (%direction% == east )
   wdoor 2177 east room 2179
   wdoor 2177 west room %WE%
elseif (%direction% == west )
   wdoor 2177 east room %EE%
   wdoor 2177 west room 2188
end
~
#2199
Room Trigg for Rm 2178~
2 g 100
~
if %random.2% == 2
   set NE 2139
else
   set NE 2142
end
eval EE (%random.2% + 2130)
eval SE (%random.2% + 2139)
eval WE (%random.2% + 2140)
wecho The trees seem to shift before your gaze.
if (%direction% == north )
   wdoor 2178 north room 2179
   wdoor 2178 east room %EE%
   wdoor 2178 south room %SE%
   wdoor 2178 west room %WE%
elseif (%direction% == east )
   wdoor 2178 north room %NE%
   wdoor 2178 east room 2190
   wdoor 2178 south room %SE%
   wdoor 2178 west room %WE%
elseif (%direction% == south )
   wdoor 2178 north room %NE%
   wdoor 2178 east room %EE%
   wdoor 2178 south room 2188
   wdoor 2178 west room %WE%
elseif (%direction% == west )
   wdoor 2178 north room %NE%
   wdoor 2178 east room %EE%
   wdoor 2178 south room %SE%
   wdoor 2178 west room 2180
end
~
$~
