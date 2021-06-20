from typing import List
from maze_builder.types import Room
import numpy as np

def compute_intersection_cost(room_arrays: List[np.array], state: np.array, map_x: int, map_y: int) -> int:
    multiplicity = np.zeros([map_x, map_y], dtype=int)
    for k, arr in enumerate(room_arrays):
        room_map = arr[0, :, :]
        room_x = state[k, 0]
        room_y = state[k, 1]
        width = room_map.shape[0]
        height = room_map.shape[1]
        multiplicity[room_x:(room_x + width), room_y:(room_y + height)] += room_map
    intersection_cost = np.sum(np.maximum(multiplicity - 1, 0))
    return int(intersection_cost)

def compute_reward(room_arrays: List[np.array], state: np.array, map_x: int, map_y: int) -> int:
    intersection_cost = compute_intersection_cost(room_arrays, state, map_x, map_y)
    total_cost = intersection_cost
    print(total_cost)
    return -total_cost
    #
    #     for i in range(room.height):
    #         for j in range(room.width):
    #             x = state[k, i]
    #             multiplicity[state[]]
    #             if room.map[i][j] == 1:
    #                 c = inverted_colors[ys[k] + i][xs[k] + j]
    return 0